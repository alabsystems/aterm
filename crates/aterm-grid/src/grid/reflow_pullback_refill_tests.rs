// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{ImageData, ImageFormat, ImageScaling};
use aterm_scrollback::{HyperlinkSpan, ImageSpan};
use std::sync::Arc;

#[test]
fn history_refills_do_not_rescan_unrelated_extras_per_row() {
    let mut excessive = Vec::new();
    for rows in [16_u16, 64, 128] {
        let cols = 16;
        let pull = rows / 2;
        let image = Arc::new(ImageData {
            bytes: vec![7; 16],
            format: ImageFormat::Png,
            cols,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });
        let make_line = |ch: char| {
            let mut line = Line::with_hyperlinks_owned(
                ch.to_string().repeat(usize::from(cols)),
                aterm_rle::Rle::with_value(aterm_scrollback::CellAttrs::DEFAULT, u32::from(cols)),
                vec![HyperlinkSpan::new(
                    0,
                    cols,
                    Arc::from("https://refill.example"),
                )],
            );
            line.set_images(vec![ImageSpan::new(0, cols, 0, 0, Arc::clone(&image))]);
            line
        };
        let history = vec![make_line('H'); usize::from(pull)];
        for (deficit, partial) in [(false, false), (true, false), (false, true)] {
            let mut grid = Grid::with_scrollback(rows, cols, usize::from(rows) * 2);
            grid.scroll_in_history(&history);
            let mut visible = vec![make_line('V'); usize::from(rows)];
            if deficit {
                visible[usize::from(pull)..].fill(Line::new());
            }
            grid.restore_visible_from_lines(&visible);
            let (top, bottom) = if partial {
                (2, rows - 3)
            } else {
                (0, rows - 1)
            };
            grid.set_scroll_region(top, bottom);
            grid.set_cursor(pull - 1, 0);
            crate::test_counters::take_extras_clear_ops();
            let restored = if deficit {
                grid.fill_viewport_deficit_from_history(0)
            } else {
                grid.unscroll_from_scrollback(usize::from(pull))
            };
            let work = crate::test_counters::take_extras_clear_ops();
            assert_eq!(restored, usize::from(pull));
            let mut expected = visible[..usize::from(top)].to_vec();
            expected.extend_from_slice(&history);
            expected.extend_from_slice(&visible[usize::from(top)..usize::from(bottom + 1 - pull)]);
            expected.extend_from_slice(&visible[usize::from(bottom + 1)..]);
            let mut reference = Grid::new(rows, cols);
            reference.restore_visible_from_lines(&expected);
            assert_eq!(
                aterm_scrollback::serialize_lines(&grid.checkpoint_visible_lines()),
                aterm_scrollback::serialize_lines(&reference.checkpoint_visible_lines())
            );
            if work > usize::from(rows) * usize::from(cols) {
                excessive.push((rows, deficit, partial, work));
            }
        }
    }
    assert!(
        excessive.is_empty(),
        "row refills revisited extras: {excessive:?}"
    );
}
