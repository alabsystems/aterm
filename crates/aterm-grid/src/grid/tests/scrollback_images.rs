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

#[test]
fn history_image_removal_releases_ring_and_cached_staged_payloads() {
    for ring in [0, 128] {
        let mut grid = Grid::with_tiered_scrollback(
            4,
            20,
            ring,
            aterm_scrollback::Scrollback::with_defaults(),
        );
        grid.set_compress_offload_active(true);
        grid.write_char('t');
        let image = placement(1, 2);
        stamp(&mut grid, 0, &image, 0, 2);
        grid.scroll_up(1);
        assert_eq!(grid.ring_buffer_scrollback() > 0, ring > 0);
        assert!(grid.history_any_image(&|_| true));
        // This also materializes and caches the lazy row before deleting its source spans.
        assert_eq!(grid.get_history_line(0).unwrap().image_count(), 1);
        let text = grid.get_history_line(0).unwrap().to_string();
        let generation = grid.content_gen();
        let bytes = grid.storage.ring_extras.retained_bytes();
        let staged = grid.storage.lazy_buffer.staged_bytes();
        let references = Arc::strong_count(&image);
        assert!(!grid.retain_history_images(&mut |_| true));
        assert_eq!(
            grid.content_gen(),
            generation,
            "no removal needs no invalidation"
        );
        assert_eq!(grid.storage.ring_extras.retained_bytes(), bytes);
        assert_eq!(grid.storage.lazy_buffer.staged_bytes(), staged);
        assert!(grid.retain_history_images(&mut |_| false));
        assert!(!grid.history_any_image(&|_| true));
        assert_eq!(
            grid.get_history_line(0).unwrap().image_count(),
            0,
            "the cached materialization cannot resurrect a tile"
        );
        assert_eq!(grid.get_history_line(0).unwrap().to_string(), text);
        assert_eq!(
            Arc::strong_count(&image),
            references - if ring == 0 { 2 } else { 1 },
            "the history span and staged materialization release their own references"
        );
        assert!(grid.content_gen() > generation);
        grid.storage.ring_extras.assert_running_sums();
        if ring == 0 {
            assert!(grid.storage.lazy_buffer.staged_bytes() < staged);
        } else {
            assert!(grid.storage.ring_extras.retained_bytes() < bytes);
        }
        // Full-screen scroll's O(1) extras offset can retain off-grid cell references
        // until its next compaction; those are separate from the history's custody.
        drop(grid);
        assert_eq!(Arc::strong_count(&image), 1);
    }
}

#[test]
fn image_carry_roundtrips_visible_ring_staged_and_hot_history_and_preserves_z_order() {
    let build_plain_tiers = || {
        let sb = aterm_scrollback::Scrollback::new(100, 1000, 100_000_000);
        let mut g = Grid::with_tiered_scrollback(4, 12, 2, sb);
        g.set_compress_offload_active(true);
        for tag in ['H', 'L', 'P', 'R'] {
            g.set_cursor(0, 0);
            g.write_char(tag);
            g.scroll_up(1);
        }
        // Promote oldest ('H', rev=3) to hot tier, leave 'L' (rev=2) staged, and 'P'/'R' (rev=1/0) in ring.
        assert_eq!(g.drain_lazy_bounded(1), 1);
        assert_eq!(g.storage.ring_buffer_scrollback(), 2);
        assert_eq!(g.lazy_backlog_len(), 1);
        assert_eq!(g.storage.scrollback.as_ref().unwrap().line_count(), 1);
        g.set_cursor(1, 0);
        g.write_char('V');
        g
    };

    let mut src = Grid::with_tiered_scrollback(
        4,
        12,
        2,
        aterm_scrollback::Scrollback::new(100, 1000, 100_000_000),
    );
    src.set_compress_offload_active(true);
    let hot_img = placement(1, 3);
    let lazy_img = placement(1, 2);
    let ring_img = placement(1, 4);
    for (tag, img) in [
        ('H', Some((&hot_img, 3u16))),
        ('L', Some((&lazy_img, 2))),
        ('P', None),
        ('R', Some((&ring_img, 4))),
    ] {
        src.set_cursor(0, 0);
        src.write_char(tag);
        if let Some((image, cols)) = img {
            stamp(&mut src, 0, image, 0, cols);
        }
        src.scroll_up(1);
    }
    assert_eq!(src.drain_lazy_bounded(1), 1);
    src.set_cursor(1, 0);
    src.write_char('V');

    // Stamp two overlapping non-Kitty layers across all 12 columns, with the upper layer ending
    // earlier (col 6) than the lower layer (col 12): tests both birth-order z preservation and
    // multi-layer 'img_flag budgeting (`cols * MAX_IMAGE_LAYERS`).
    let under = placement(2, 12);
    let over = placement(1, 6);
    for col in 0..12u16 {
        src.stack_cell_image(
            1,
            col,
            ImageRef {
                image: Arc::clone(&under),
                cell_row: 0,
                cell_col: col,
                kitty: None,
            },
        );
    }
    for col in 0..6u16 {
        src.stack_cell_image(
            1,
            col,
            ImageRef {
                image: Arc::clone(&over),
                cell_row: 0,
                cell_col: col,
                kitty: None,
            },
        );
    }

    let carry = src.capture_image_carry(usize::MAX);
    assert_eq!(carry.visible.len(), 1);
    assert_eq!(carry.visible[0].0, 1);
    assert_eq!(
        carry
            .history_rev
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>(),
        vec![0, 2, 3],
    );

    let mut dst = build_plain_tiers();
    assert!(!dst.extras().any_image());
    assert!(!dst.history_any_image(&|_| true));
    // Materialize the lazy line before applying carry to prove cached materializations invalidate.
    assert_eq!(dst.history_line_rev(2).unwrap().image_count(), 0);

    dst.apply_image_carry(&carry);
    dst.storage.ring_extras.assert_running_sums();

    for col in 0..12u16 {
        assert!(
            dst.cell(1, col).unwrap().has_extras(),
            "col {col} must mark has_extras across full width"
        );
        let stack = dst.extras().get(CellCoord::new(1, col)).unwrap().images();
        if col < 6 {
            assert_eq!(stack.len(), 2, "overlap at col {col}");
            assert!(
                Arc::ptr_eq(&stack[0].image, &under) && Arc::ptr_eq(&stack[1].image, &over),
                "birth z-order preserved at col {col}"
            );
        } else {
            assert_eq!(stack.len(), 1, "tail at col {col}");
            assert!(Arc::ptr_eq(&stack[0].image, &under));
        }
    }
    assert!(Arc::ptr_eq(
        &dst.history_line_rev(0).unwrap().images().unwrap()[0].image,
        &ring_img
    ));
    assert!(dst.history_line_rev(1).unwrap().images().is_none());
    assert!(Arc::ptr_eq(
        &dst.history_line_rev(2).unwrap().images().unwrap()[0].image,
        &lazy_img
    ));
    assert!(Arc::ptr_eq(
        &dst.history_line_rev(3).unwrap().images().unwrap()[0].image,
        &hot_img
    ));
    assert_eq!(dst.capture_image_carry(usize::MAX), carry);

    // Bounded carry truncates history_rev cleanly; empty carry clears all tiers and visible cells.
    assert_eq!(
        carry
            .bounded(3)
            .history_rev
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    dst.apply_image_carry(&crate::GridImageCarry::default());
    dst.storage.ring_extras.assert_running_sums();
    assert!(dst.capture_image_carry(usize::MAX).is_empty());
    assert!(!dst.history_any_image(&|_| true));
}
