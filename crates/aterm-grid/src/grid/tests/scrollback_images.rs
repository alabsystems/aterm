// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inline images survive the scroll-off -> history -> materialize round trip.

use std::sync::Arc;

use super::super::*;
use crate::{CellExtras, ImageData, ImageFormat, ImageRef, ImageScaling};

fn placement(rows: u16, cols: u16) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes: vec![7u8; 256],
        format: ImageFormat::Png,
        cols,
        rows,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

fn stamp(grid: &mut Grid, row_idx: u16, image: &Arc<ImageData>, image_row: u16, cols: u16) {
    for cell_col in 0..cols {
        grid.set_cell_image(
            row_idx,
            cell_col,
            ImageRef {
                image: Arc::clone(image),
                cell_row: image_row,
                cell_col,
                kitty: None,
            },
        );
    }
}

fn scrolled_extras(grid: &Grid, row_idx: u16) -> ScrolledRowExtras {
    let row = grid.row(row_idx).expect("row exists");
    Grid::extract_row_extras(row, grid.extras(), row_idx)
}

#[test]
fn a_scrolled_off_image_row_round_trips_through_a_line_back_to_per_cell_refs() {
    let mut grid = Grid::new(4, 20);
    let image = placement(3, 5);
    stamp(&mut grid, 1, &image, 2, 5);

    let row = grid.row(1).unwrap();
    let extras = scrolled_extras(&grid, 1);
    let line = Grid::row_to_line_with_stored_extras(row, &extras);
    assert_eq!(
        line.image_count(),
        1,
        "the history line carries the picture"
    );

    let mat = scroll_materialize::materialize_from_line(&line, 20);
    for col in 0..5u16 {
        let iref = mat
            .get_extra(col)
            .and_then(|e| e.image())
            .unwrap_or_else(|| panic!("column {col} lost its image"));
        assert_eq!(iref.cell_row, 2, "footprint row preserved at col {col}");
        assert_eq!(
            iref.cell_col, col,
            "footprint column preserved at col {col}"
        );
        assert!(
            Arc::ptr_eq(&iref.image, &image),
            "restored col {col} must share the ONE payload"
        );
    }
    assert!(
        mat.get_extra(5).and_then(|e| e.image()).is_none(),
        "the footprint must not spill past its width"
    );
}

#[test]
fn an_image_row_still_materializes_as_empty_text() {
    let mut grid = Grid::new(4, 20);
    stamp(&mut grid, 0, &placement(1, 6), 0, 6);
    let extras = scrolled_extras(&grid, 0);
    let line = Grid::row_to_line_with_stored_extras(grid.row(0).unwrap(), &extras);
    assert_eq!(line.as_str(), Some(""));
    let mat = scroll_materialize::materialize_from_line(&line, 20);
    assert_eq!(mat.len(), 0, "an image row's occupied length stays 0");
}

#[test]
fn the_ring_fast_materializer_agrees_with_the_line_round_trip_on_an_image_row() {
    let mut grid = Grid::new(4, 20);
    let image = placement(2, 4);
    stamp(&mut grid, 0, &image, 1, 4);
    let extras = scrolled_extras(&grid, 0);
    let row = grid.row(0).unwrap();

    let via_line = scroll_materialize::materialize_from_line(
        &Grid::row_to_line_with_stored_extras(row, &extras),
        20,
    );
    let fast = scroll_materialize::materialize_from_row_extras(row, Some(&extras), 20)
        .expect("the fast path handles a zero-length row");
    assert_eq!(
        fast, via_line,
        "the ring fast path and the Line round trip disagree on an image row"
    );
}

#[test]
fn a_run_broken_by_a_second_placement_splits_into_two_spans() {
    let mut grid = Grid::new(4, 20);
    let left = placement(1, 2);
    let right = placement(1, 2);
    stamp(&mut grid, 0, &left, 0, 2);
    for cell_col in 0..2u16 {
        grid.set_cell_image(
            0,
            2 + cell_col,
            ImageRef {
                image: Arc::clone(&right),
                cell_row: 0,
                cell_col,
                kitty: None,
            },
        );
    }
    let extras = scrolled_extras(&grid, 0);
    assert_eq!(extras.images.len(), 2, "two placements, two spans");
    assert_eq!(
        (extras.images[0].start_col, extras.images[0].end_col),
        (0, 2)
    );
    assert_eq!(
        (extras.images[1].start_col, extras.images[1].end_col),
        (2, 4)
    );
    assert!(Arc::ptr_eq(&extras.images[1].image, &right));
}

#[test]
fn a_grid_that_never_drew_a_picture_never_scans_for_one() {
    let extras = CellExtras::default();
    assert!(!extras.any_image(), "a fresh collection has no image");
    let mut grid = Grid::new(4, 20);
    grid.write_char('x');
    assert!(
        !grid.extras().any_image(),
        "plain text must not arm the image scan"
    );
    stamp(&mut grid, 1, &placement(1, 1), 0, 1);
    assert!(
        grid.extras().any_image(),
        "placing a picture must arm the scan that carries it into history"
    );
}
