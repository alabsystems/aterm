// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LineSizeSpan, RenderInput};
use crate::grid::LineSize;

fn input(rows: usize, cols: usize) -> RenderInput {
    let mut i = RenderInput::empty();
    i.rows = rows;
    i.cols = cols;
    i.line_sizes = vec![LineSize::SingleWidth; rows];
    i.line_size_spans = vec![Vec::new(); rows];
    i
}

#[test]
fn empty_spans_yield_the_row_line_size_over_the_whole_row() {
    let mut i = input(2, 10);
    i.line_sizes[1] = LineSize::DoubleWidth;
    assert_eq!(i.line_size_run_at(0, 0), (LineSize::SingleWidth, 0, 10));
    assert_eq!(i.line_size_run_at(0, 9), (LineSize::SingleWidth, 0, 10));
    assert_eq!(i.line_size_run_at(1, 4), (LineSize::DoubleWidth, 0, 10));
}

#[test]
fn run_boundaries_are_half_open_and_gaps_fall_back() {
    let mut i = input(1, 12);
    i.line_size_spans[0] = vec![
        LineSizeSpan {
            start_col: 0,
            end_col: 3,
            line_size: LineSize::DoubleHeightTop,
        },
        LineSizeSpan {
            start_col: 8,
            end_col: 12,
            line_size: LineSize::DoubleWidth,
        },
    ];
    assert_eq!(i.line_size_run_at(0, 0).0, LineSize::DoubleHeightTop);
    assert_eq!(i.line_size_run_at(0, 2).0, LineSize::DoubleHeightTop);
    assert_eq!(i.line_size_run_at(0, 3), (LineSize::SingleWidth, 0, 12));
    assert_eq!(i.line_size_run_at(0, 7), (LineSize::SingleWidth, 0, 12));
    assert_eq!(i.line_size_run_at(0, 8).0, LineSize::DoubleWidth);
    assert_eq!(i.line_size_run_at(0, 11).0, LineSize::DoubleWidth);
}

#[test]
fn a_spanned_row_never_inherits_the_row_level_summary() {
    let mut i = input(1, 11);
    i.line_sizes[0] = LineSize::DoubleWidth;
    i.line_size_spans[0] = vec![LineSizeSpan {
        start_col: 6,
        end_col: 11,
        line_size: LineSize::DoubleWidth,
    }];
    for c in 0..6 {
        assert_eq!(
            i.line_size_run_at(0, c),
            (LineSize::SingleWidth, 0, 11),
            "column {c} is unclaimed composite space, not the summary's width"
        );
    }
    assert_eq!(i.line_size_run_at(0, 6), (LineSize::DoubleWidth, 6, 11));
}

#[test]
fn out_of_range_lookups_fall_back_instead_of_panicking() {
    let mut i = input(1, 4);
    i.line_size_spans[0] = vec![LineSizeSpan {
        start_col: 0,
        end_col: 2,
        line_size: LineSize::DoubleWidth,
    }];
    assert_eq!(i.line_size_run_at(0, 99), (LineSize::SingleWidth, 0, 4));
    assert_eq!(i.line_size_run_at(9, 0), (LineSize::SingleWidth, 0, 4));
}
