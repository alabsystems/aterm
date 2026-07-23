// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::{CellCoord, Grid};

/// Regression: scroll_region_up must shift CellExtras within the region (#1967).
/// Before fix, hyperlinks would desync from their visual rows after region scroll.
#[test]
fn grid_scroll_region_up_shifts_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::new(8, 10);
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Place hyperlinks: row 1 (outside), row 3 (inside region), row 6 (outside)
    let url_outside_above: Arc<str> = Arc::from("https://above.com");
    let url_inside: Arc<str> = Arc::from("https://inside.com");
    let url_outside_below: Arc<str> = Arc::from("https://below.com");

    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 0))
        .set_hyperlink(Some(url_outside_above.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(3, 0))
        .set_hyperlink(Some(url_inside.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(6, 0))
        .set_hyperlink(Some(url_outside_below.clone()));

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region up by 1
    grid.scroll_region_up(1);

    // Row 1 (outside, above): hyperlink preserved at row 1
    assert!(grid.extras().row_has_hyperlinks(1));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(1, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_outside_above)
    );

    // Row 3 was inside region, scrolled up by 1 → now at row 2
    assert!(grid.extras().row_has_hyperlinks(2));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(2, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_inside)
    );
    // Row 3 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(3));

    // Row 6 (outside, below): hyperlink preserved at row 6
    assert!(grid.extras().row_has_hyperlinks(6));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(6, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_outside_below)
    );
}

/// The top-anchored archival path must split metadata in two directions: row
/// zero's extras enter history, while extras on the protected footer stay at
/// their visible coordinates.
#[test]
fn grid_top_anchored_partial_scroll_preserves_history_and_footer_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(5, 10, 10);
    for row in 0..5 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    let history_url: Arc<str> = Arc::from("https://history.example");
    let footer_url: Arc<str> = Arc::from("https://footer.example");
    grid.extras_mut()
        .get_or_create(CellCoord::new(0, 0))
        .set_hyperlink(Some(history_url.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(4, 0))
        .set_hyperlink(Some(footer_url.clone()));

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

    assert_eq!(
        grid.extras()
            .get(CellCoord::new(4, 0))
            .and_then(|extra| extra.hyperlink().cloned()),
        Some(footer_url)
    );
    assert!(
        !grid.extras().row_has_hyperlinks(3),
        "protected footer metadata must not remain at its temporary shifted row"
    );
    grid.assert_invariants();
}

/// Regression: scroll_region_down must shift CellExtras within the region (#1967).
#[test]
fn grid_scroll_region_down_shifts_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::new(8, 10);
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Place hyperlink inside region at row 3
    let url_inside: Arc<str> = Arc::from("https://inside.com");
    let url_outside: Arc<str> = Arc::from("https://outside.com");

    grid.extras_mut()
        .get_or_create(CellCoord::new(3, 0))
        .set_hyperlink(Some(url_inside.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(7, 0))
        .set_hyperlink(Some(url_outside.clone()));

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Scroll region down by 1
    grid.scroll_region_down(1);

    // Row 3 hyperlink should have shifted down to row 4
    assert!(grid.extras().row_has_hyperlinks(4));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(4, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_inside)
    );
    // Row 3 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(3));

    // Row 7 (outside): preserved
    assert!(grid.extras().row_has_hyperlinks(7));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(7, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_outside)
    );
}

/// Regression: insert_lines must shift CellExtras down within [cursor_row, region.bottom] (#1981).
#[test]
fn grid_insert_lines_shifts_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::new(8, 10);
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Place hyperlinks: row 1 (outside above), row 4 (inside, will shift), row 7 (outside below)
    let url_above: Arc<str> = Arc::from("https://above.com");
    let url_inside: Arc<str> = Arc::from("https://inside.com");
    let url_below: Arc<str> = Arc::from("https://below.com");

    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 0))
        .set_hyperlink(Some(url_above.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(4, 0))
        .set_hyperlink(Some(url_inside.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(7, 0))
        .set_hyperlink(Some(url_below.clone()));

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 3, insert 1 line — shifts rows 3-5 down by 1
    grid.set_cursor(3, 0);
    grid.insert_lines(1);

    // Row 1 (outside above): preserved
    assert!(grid.extras().row_has_hyperlinks(1));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(1, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_above)
    );

    // Row 4 hyperlink was at row 4, shifted down to row 5
    assert!(grid.extras().row_has_hyperlinks(5));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(5, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_inside)
    );
    // Row 4 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(4));

    // Row 7 (outside below): preserved
    assert!(grid.extras().row_has_hyperlinks(7));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(7, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_below)
    );
}

/// Regression: delete_lines must shift CellExtras up within [cursor_row, region.bottom] (#1981).
#[test]
fn grid_delete_lines_shifts_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::new(8, 10);
    for row in 0..8 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Place hyperlinks: row 1 (outside above), row 4 (inside, will shift), row 7 (outside below)
    let url_above: Arc<str> = Arc::from("https://above.com");
    let url_inside: Arc<str> = Arc::from("https://inside.com");
    let url_below: Arc<str> = Arc::from("https://below.com");

    grid.extras_mut()
        .get_or_create(CellCoord::new(1, 0))
        .set_hyperlink(Some(url_above.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(4, 0))
        .set_hyperlink(Some(url_inside.clone()));
    grid.extras_mut()
        .get_or_create(CellCoord::new(7, 0))
        .set_hyperlink(Some(url_below.clone()));

    // Set scroll region: rows 2-5
    grid.set_scroll_region(2, 5);

    // Cursor at row 3, delete 1 line — shifts rows 4-5 up by 1
    grid.set_cursor(3, 0);
    grid.delete_lines(1);

    // Row 1 (outside above): preserved
    assert!(grid.extras().row_has_hyperlinks(1));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(1, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_above)
    );

    // Row 4 hyperlink shifted up to row 3
    assert!(grid.extras().row_has_hyperlinks(3));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(3, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_inside)
    );
    // Row 4 should no longer have the hyperlink
    assert!(!grid.extras().row_has_hyperlinks(4));

    // Row 7 (outside below): preserved
    assert!(grid.extras().row_has_hyperlinks(7));
    assert_eq!(
        grid.extras()
            .get(CellCoord::new(7, 0))
            .and_then(|e| e.hyperlink().cloned()),
        Some(url_below)
    );
}
