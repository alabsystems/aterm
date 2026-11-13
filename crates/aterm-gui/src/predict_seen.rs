// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cells predictive echo reads, projected to [`Seen`].

use aterm_core::grid::LineSize;
use aterm_core::render::RenderInput;
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::Seen;

fn seen_of(
    cells: &[RenderCell],
    marked: impl Fn(usize) -> bool,
    row_opaque: bool,
    col: usize,
) -> Seen {
    let Some(cell) = cells.get(col) else {
        return Seen::Blank;
    };
    let opaque = row_opaque
        || cell.wide
        || cells
            .get(col.saturating_add(1))
            .is_some_and(|next| next.wide)
        || marked(col);
    Seen::of_cell(cell.ch, opaque)
}

fn has_marks(
    clusters: &[(usize, Box<str>)],
    combining: &[(usize, Box<[char]>)],
    col: usize,
) -> bool {
    clusters.iter().any(|(at, _)| *at == col) || combining.iter().any(|(at, _)| *at == col)
}

/// What the reconcile's observe answers at LOGICAL column `col` of frame row `row` of an extracted
/// frame (the single-pane `input_scratch`, or the focused pane's `pane_scratch` before the composed
/// blit).
pub(crate) fn seen_at(input: &RenderInput, row: u16, col: u16) -> Seen {
    let r = usize::from(row);
    let frame_col = input.visual_col(r, usize::from(col));
    let cells = input.cells.get(r).map_or(&[][..], Vec::as_slice);
    let clusters = input.clusters.get(r).map_or(&[][..], Vec::as_slice);
    let combining = input.combining.get(r).map_or(&[][..], Vec::as_slice);
    let row_opaque = input
        .visual_to_logical
        .get(r)
        .is_some_and(|map| !map.is_empty())
        || input
            .line_sizes
            .get(r)
            .is_some_and(|size| *size != LineSize::SingleWidth)
        || input
            .line_size_spans
            .get(r)
            .is_some_and(|spans| !spans.is_empty());
    seen_of(
        cells,
        |c| has_marks(clusters, combining, c),
        row_opaque,
        frame_col,
    )
}

/// The cursor row as a keypress hands it to the predictor, with the buffers its extraction fills.
#[derive(Default)]
pub(crate) struct KeyRow {
    cells: Vec<RenderCell>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
    /// One [`Seen`] per logical column of the row last read.
    seen: Vec<Seen>,
    /// The row under it, read the same way, empty when it was the last row.
    below: Vec<Seen>,
}

impl KeyRow {
    /// The row last read by [`seen_row`], one cell per logical column.
    pub(crate) fn cells(&self) -> &[Seen] {
        &self.seen
    }

    /// The row under it, as [`seen_row`] read it; `None` when that was the last row.
    pub(crate) fn below(&self) -> Option<&[Seen]> {
        (!self.below.is_empty()).then_some(self.below.as_slice())
    }
}

/// Read visible row `row` of `t` into `out` as the predictor's key-time sample.
pub(crate) fn seen_row(t: &Terminal, row: u16, out: &mut KeyRow) {
    out.below.clear();
    if row.saturating_add(1) < t.rows() {
        read_row(t, row.saturating_add(1), out);
        std::mem::swap(&mut out.seen, &mut out.below);
    }
    read_row(t, row, out);
}

fn read_row(t: &Terminal, row: u16, out: &mut KeyRow) {
    let logical = t.logical_row_into(
        usize::from(row),
        &mut out.cells,
        &mut out.clusters,
        &mut out.combining,
    );
    let (cells, clusters, combining) = (&out.cells, &out.clusters, &out.combining);
    out.seen.clear();
    out.seen.extend(
        (0..usize::from(t.cols()))
            .map(|c| seen_of(cells, |at| has_marks(clusters, combining, at), !logical, c)),
    );
}

#[cfg(test)]
#[path = "predict_seen_tests.rs"]
mod tests;
