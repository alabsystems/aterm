// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cells predictive echo reads, projected to [`Seen`]: at a keypress, the
//! cursor row the press hands `Predictor::predict_glyph` ([`seen_row`]), and at
//! a frame, the cell the reconcile's observe answers ([`seen_at`]).
//!
//! The predictor's TAIL GUARD compares the two: the row right of a guess as the
//! press saw it, with the same row once the guess's echo landed. An echo that
//! left the row as it was (a right prompt, a suggestion the typed glyph matched)
//! proves the line; one that moved it (a mid-line insert) proves nothing. So the
//! two projections must agree about every cell that did not change, or an
//! unchanged right prompt reads as a change and the line stays dark. Both come
//! from the same extraction (`Terminal::logical_row_into` is the frame's own row
//! extraction, before BiDi reordering) and both go through [`seen_of`].
//!
//! They agree on every cell but a bracket at a right-to-left level: the frame
//! draws it with its mirroring glyph (UAX #9 L4, `aterm-core`'s
//! `apply_bidi_reorder`), and the key row, read before the reordering, does not.
//! Both cells are opaque, and the disagreement reads as a change, so a line with
//! such a bracket right of the cursor fails closed: it never proves.
//!
//! A cell is OPAQUE ([`Seen::Opaque`]) when its glyph and column do not describe
//! it: either half of a wide glyph, a grapheme cluster or combining marks, and
//! every cell of a row the frame draws in another order (BiDi) or at another
//! size (DEC double width or height). Opaque cells are compared only for
//! equality; the glyph lane reads them as the plain glyph they carry.

use aterm_core::grid::LineSize;
use aterm_core::render::RenderInput;
use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::Seen;

/// One cell of an extracted row in the frame's column order: its glyph, opaque
/// on either half of a wide glyph, on marks (`marked`), or anywhere on a row
/// the frame draws in another order or size (`row_opaque`). A column past a
/// trimmed row is the implicit blank.
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

/// Whether column `col` of a row carries a grapheme cluster or combining marks.
fn has_marks(
    clusters: &[(usize, Box<str>)],
    combining: &[(usize, Box<[char]>)],
    col: usize,
) -> bool {
    clusters.iter().any(|(at, _)| *at == col) || combining.iter().any(|(at, _)| *at == col)
}

/// What the reconcile's observe answers at LOGICAL column `col` of frame row
/// `row` of an extracted frame (the single-pane `input_scratch`, or the focused
/// pane's `pane_scratch` before the composed blit). The column is found through
/// `visual_col`, as the ghost painter finds it, and the glyph it carries is what
/// the plain observe answered before, so the glyph lane reads the frame as it
/// always did.
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

/// The cursor row as a keypress hands it to the predictor, with the buffers its
/// extraction fills. Resident per window, so a run's first press refills it in
/// place; type-ahead never reads it (`Predictor::wants_key_row`).
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

    /// The row under it, as [`seen_row`] read it (`KeySample::below`, THE
    /// FLOOR a run's proof stands on); `None` when that was the last row.
    pub(crate) fn below(&self) -> Option<&[Seen]> {
        (!self.below.is_empty()).then_some(self.below.as_slice())
    }
}

/// Read visible row `row` of `t` into `out` as the predictor's key-time sample:
/// one [`Seen`] per column of the grid, projected exactly as [`seen_at`] projects
/// the same row of a frame. The row under it too, read the same way, unless
/// `row` is the last. Call under the press's terminal hold, beside the
/// cursor read.
pub(crate) fn seen_row(t: &Terminal, row: u16, out: &mut KeyRow) {
    // The row under it first, read into `seen` and swapped into `below`: both
    // buffers stay resident.
    out.below.clear();
    if row.saturating_add(1) < t.rows() {
        read_row(t, row.saturating_add(1), out);
        std::mem::swap(&mut out.seen, &mut out.below);
    }
    read_row(t, row, out);
}

/// Read visible row `row` of `t` into `out.seen`, one [`Seen`] per column.
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
mod tests {
    use super::*;

    /// A terminal of one 12-column row holding `bytes`.
    fn row_of(bytes: &[u8]) -> Terminal {
        let mut term = Terminal::new(2, 12);
        term.process(bytes);
        term
    }

    /// The press's row and the frame's observe agree on every cell, for rows
    /// holding a wide glyph, a combining mark, an emoji cluster, a right prompt
    /// in italics, and right-to-left text the frame reorders; and they mark the
    /// cells they must as opaque.
    #[test]
    fn the_key_row_and_the_frame_observe_agree_cell_by_cell() {
        let rows: [(&str, &[u8]); 6] = [
            ("plain", b"$ ab cd"),
            ("right prompt", b"$ ab\x1b[9G\x1b[3m<r>\x1b[23m"),
            ("wide glyph", "$ a日b".as_bytes()),
            ("combining", "$ e\u{301}x".as_bytes()),
            ("emoji cluster", "$ 👍🏽 ok".as_bytes()),
            ("right to left", "$ \u{5d0}\u{5d1} ab".as_bytes()),
        ];
        for (label, bytes) in rows {
            let mut term = row_of(bytes);
            let mut key = KeyRow::default();
            seen_row(&term, 0, &mut key);
            let mut frame = RenderInput::empty();
            term.cell_frame_into(&mut frame, 2, 12);
            let observed: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 0, c)).collect();
            assert_eq!(key.cells(), observed.as_slice(), "{label}");
            let opaque = key
                .cells()
                .iter()
                .filter(|s| matches!(s, Seen::Opaque(_)))
                .count();
            let expected_opaque = match label {
                "plain" | "right prompt" => 0,
                "wide glyph" => 2,
                "combining" => 1,
                "emoji cluster" => 2,
                // Every written cell (`$ אב ab`, the Hebrew pair wide or not): no
                // cell of the row is at its logical column. Past the written
                // prefix both orders hold the implicit blank.
                _ => 7,
            };
            assert_eq!(opaque, expected_opaque, "{label}: {:?}", key.cells());
        }
    }

    /// The press also reads the row UNDER the cursor row (`KeySample::below`, the
    /// predictor's floor), projected as the frame's observe projects that row,
    /// and none under the last row.
    #[test]
    fn the_key_row_reads_the_row_under_it_as_the_frame_does() {
        let mut term = row_of("$ ab\r\nPassword: \x1b[A\x1b[5G".as_bytes());
        let mut key = KeyRow::default();
        seen_row(&term, 0, &mut key);
        let mut frame = RenderInput::empty();
        term.cell_frame_into(&mut frame, 2, 12);
        let under: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 1, c)).collect();
        assert_eq!(key.below(), Some(under.as_slice()));
        let top: Vec<Seen> = (0..12).map(|c| seen_at(&frame, 0, c)).collect();
        assert_eq!(
            key.cells(),
            top.as_slice(),
            "the cursor row is still the row read"
        );
        seen_row(&term, 1, &mut key);
        assert_eq!(key.below(), None, "no row under the last");
        assert_eq!(key.cells(), under.as_slice());
    }

    /// The one cell the projections disagree on: a bracket inside right-to-left
    /// text, which the frame draws mirrored and the key row, read before the
    /// reordering, does not. Each such cell is opaque on both sides and holds the
    /// other side's mirror image; every other cell agrees. So the tail guard sees
    /// a change there and the line fails closed.
    #[test]
    fn a_mirrored_bracket_is_the_one_cell_they_disagree_on() {
        let mut term = row_of("$ \u{5d0}(\u{5d1}) x".as_bytes());
        let mut key = KeyRow::default();
        seen_row(&term, 0, &mut key);
        let mut frame = RenderInput::empty();
        term.cell_frame_into(&mut frame, 2, 12);
        let mirror = |s: Seen| match s {
            Seen::Opaque(Some('(')) => Seen::Opaque(Some(')')),
            Seen::Opaque(Some(')')) => Seen::Opaque(Some('(')),
            other => other,
        };
        let mut mirrored = 0;
        for c in 0..12u16 {
            let k = key
                .cells()
                .get(usize::from(c))
                .copied()
                .unwrap_or(Seen::Blank);
            let f = seen_at(&frame, 0, c);
            if k != f {
                assert_eq!(mirror(k), f, "column {c}: only a mirrored bracket differs");
                mirrored += 1;
            }
        }
        assert_eq!(
            mirrored,
            2,
            "both brackets, and nothing else: {:?}",
            key.cells()
        );
    }

    /// The glyph each projection carries is what the plain observe answered: the
    /// cell's char, `None` for a blank or a wide glyph's right half.
    #[test]
    fn an_opaque_cell_carries_the_plain_answer() {
        let term = row_of("$ a日b".as_bytes());
        let mut key = KeyRow::default();
        seen_row(&term, 0, &mut key);
        assert_eq!(key.cells().get(2), Some(&Seen::Glyph('a')));
        assert_eq!(key.cells().get(3), Some(&Seen::Opaque(Some('日'))));
        assert_eq!(key.cells().get(4), Some(&Seen::Opaque(None)));
        assert_eq!(key.cells().get(5), Some(&Seen::Glyph('b')));
        assert_eq!(key.cells().get(6), Some(&Seen::Blank));
    }
}
