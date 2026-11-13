// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::sync::Arc;

use aterm_alloc::SmallVec;
use aterm_scrollback::{HyperlinkSpan, ImageSpan};
use aterm_types::{ImageData, ImageFormat, ImageScaling};

use super::ScrolledRowExtras;

#[test]
fn consumed_spans_match_borrowed_and_cached_materialization() {
    let image = Arc::new(ImageData {
        bytes: vec![7; 16],
        format: ImageFormat::Png,
        cols: 4,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    for len in [0_u16, 4] {
        for links in [false, true] {
            for images in [false, true] {
                let mut grid = super::Grid::new(1, 4);
                let row = grid.row_mut(0).unwrap();
                for col in 0..len {
                    row.set(col, super::Cell::new('x'));
                }
                row.update_len(len);
                row.set_wrapped(true);
                let mut extras = ScrolledRowExtras::default();
                if links {
                    extras.hyperlinks.push(HyperlinkSpan::new(
                        0,
                        4,
                        Arc::from("https://example.com"),
                    ));
                }
                if images {
                    extras
                        .images
                        .push(ImageSpan::new(0, 4, 0, 0, Arc::clone(&image)));
                }
                let expected =
                    super::cells_to_line(&row.as_slice()[..usize::from(len)], &extras, true);
                for cached in [false, true] {
                    let mut staged = super::super::lazy_buffer::LazyBuffer::new();
                    staged.push_row(row, extras.clone());
                    if cached {
                        let _ = staged.get_line(0);
                    }
                    let actual = staged.drain_all().next().unwrap();
                    assert_eq!(
                        aterm_scrollback::serialize_lines(std::slice::from_ref(&actual)),
                        aterm_scrollback::serialize_lines(std::slice::from_ref(&expected))
                    );
                    assert_eq!(actual.images().is_some(), images);
                    if let Some(spans) = actual.images() {
                        assert!(Arc::ptr_eq(&spans[0].image, &image));
                    }
                    assert_eq!(actual.hyperlinks().is_some(), links && len != 0);
                }
            }
        }
    }
}

#[test]
fn is_empty_iff_every_field_is_empty() {
    let image = Arc::new(ImageData {
        bytes: vec![0x89, b'P', b'N', b'G'],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    for subset in 0u8..(1 << 7) {
        let has = |field: u8| subset >> field & 1 == 1;
        let mut extras = ScrolledRowExtras::default();
        if has(0) {
            extras
                .hyperlinks
                .push(HyperlinkSpan::new(0, 1, Arc::from("https://example.com")));
        }
        if has(1) {
            extras.complex_chars.push((0, Arc::from("abc")));
        }
        if has(2) {
            extras
                .combining
                .push((0, SmallVec::from_slice(&['\u{0300}'])));
        }
        if has(3) {
            extras.rgb_fg.push((0, [255, 0, 0]));
        }
        if has(4) {
            extras.rgb_bg.push((0, [0, 0, 255]));
        }
        if has(5) {
            extras.underline_colors.push((0, 0x01_FF_00_00));
        }
        if has(6) {
            extras
                .images
                .push(ImageSpan::new(0, 1, 0, 0, Arc::clone(&image)));
        }
        assert_eq!(extras.is_empty(), subset == 0, "fields {subset:07b}");
    }
}
