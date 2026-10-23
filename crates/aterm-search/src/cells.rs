// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The grid cells a line of indexed or selected text was read from.

use std::num::NonZeroUsize;
use std::ops::Range;

use crate::grapheme::ColumnMap;

/// The grid cells one line of text was read from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineCells {
    /// The first run.
    head: Option<CellRun>,
    /// The runs after it.
    tail: Vec<CellRun>,
    /// The text these cells were read with stops short of the line's end.
    truncated: bool,
}

/// `col + i·width`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellRun {
    byte: usize,
    col: usize,
    count: u32,
    bytes: u32,
    width: u16,
}

impl CellRun {
    #[inline]
    const fn cell(self, i: u32) -> CellSpan {
        let i = i as usize;
        let byte = self
            .byte
            .saturating_add(i.saturating_mul(self.bytes as usize));
        let col = self
            .col
            .saturating_add(i.saturating_mul(self.width as usize));
        CellSpan {
            byte,
            end: byte.saturating_add(self.bytes as usize),
            col,
            end_col: col.saturating_add(self.width as usize),
        }
    }

    /// The byte after the run's last cell.
    #[inline]
    const fn end_byte(self) -> usize {
        self.cell(self.count).byte
    }

    /// run ends in both bytes and columns.
    #[inline]
    const fn continued_by(self, next: Self) -> bool {
        let after = self.cell(self.count);
        next.bytes == self.bytes
            && next.width == self.width
            && next.byte == after.byte
            && next.col == after.col
            && self.count.checked_add(next.count).is_some()
    }
}

/// One cell of a [`LineCells`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellSpan {
    /// Where the cell's text starts in the line.
    pub(crate) byte: usize,
    /// Where it ends (exclusive).
    pub(crate) end: usize,
    /// The cell's first column.
    pub(crate) col: usize,
    /// The column after it.
    pub(crate) end_col: usize,
}

impl LineCells {
    /// No cells yet.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            head: None,
            tail: Vec::new(),
            truncated: false,
        }
    }

    /// Record that the text read with these cells stops short of the line's end.
    #[inline]
    pub const fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Whether the text read with these cells stops short of the line's end ([`mark_truncated`]).
    #[inline]
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Record that the line's bytes `text` are drawn in the cell at column `col`.
    #[inline]
    pub fn push(&mut self, text: Range<usize>, col: usize, width: usize) {
        self.push_repeated(text, col, width, 1);
    }

    /// [`push`](Self::push) `count` cells shaped like the first.
    pub fn push_repeated(&mut self, first: Range<usize>, col: usize, width: usize, count: usize) {
        let Some(bytes) = first
            .end
            .checked_sub(first.start)
            .and_then(|len| u32::try_from(len).ok())
            .filter(|&len| len > 0)
        else {
            return;
        };
        let Some(count) = u32::try_from(count).ok().filter(|&count| count > 0) else {
            return;
        };
        if self.last().is_some_and(|run| first.start < run.end_byte()) {
            return;
        }
        self.push_run(CellRun {
            byte: first.start,
            col,
            count,
            bytes,
            width: u16::try_from(width).unwrap_or(u16::MAX),
        });
    }

    /// Append `other`'s cells, `bytes` further into the line and `cols` further along it.
    pub fn append_shifted(&mut self, other: &Self, bytes: usize, cols: usize) {
        self.truncated |= other.truncated;
        for run in other.runs() {
            let shifted = CellRun {
                byte: run.byte.saturating_add(bytes),
                col: run.col.saturating_add(cols),
                ..*run
            };
            if self
                .last()
                .is_some_and(|last| shifted.byte < last.end_byte())
            {
                continue;
            }
            self.push_run(shifted);
        }
    }

    /// Append `text`.
    pub fn append_wrapped_row(
        &mut self,
        line: &mut String,
        text: &str,
        cells: &Self,
        row_start: usize,
    ) {
        for col in self.end_col()..row_start {
            let byte = line.len();
            line.push(' ');
            self.push(byte..line.len(), col, 1);
        }
        self.append_shifted(cells, line.len(), row_start);
        line.push_str(text);
    }

    fn push_run(&mut self, run: CellRun) {
        let last = match self.tail.last_mut() {
            Some(last) => Some(last),
            None => self.head.as_mut(),
        };
        if let Some(last) = last
            && last.continued_by(run)
        {
            last.count = last.count.saturating_add(run.count);
            return;
        }
        if self.head.is_none() {
            self.head = Some(run);
        } else {
            self.tail.push(run);
        }
    }

    /// The last run.
    #[inline]
    fn last(&self) -> Option<&CellRun> {
        self.tail.last().or(self.head.as_ref())
    }

    /// Every run, in line order.
    #[inline]
    fn runs(&self) -> impl Iterator<Item = &CellRun> {
        self.head.iter().chain(&self.tail)
    }

    /// The byte after the last cell's text.
    #[inline]
    #[must_use]
    pub fn end_byte(&self) -> usize {
        self.last().map_or(0, |run| run.end_byte())
    }

    /// The column after the last cell: how many columns the cells reach, `0` with none.
    #[inline]
    #[must_use]
    pub fn end_col(&self) -> usize {
        self.last()
            .map_or(0, |run| run.cell(run.count.saturating_sub(1)).end_col)
    }

    /// Whether the cells are `len` one-byte, one-column cells from column 0.
    #[inline]
    #[must_use]
    pub(crate) fn is_identity(&self, len: usize) -> bool {
        match (&self.head, self.tail.as_slice()) {
            (None, _) => len == 0,
            (Some(run), []) => {
                run.byte == 0
                    && run.col == 0
                    && run.bytes == 1
                    && run.width == 1
                    && run.count as usize == len
            }
            (Some(_), _) => false,
        }
    }

    /// `len` one-byte, one-column cells from column 0 (see [`Self::is_identity`]).
    #[must_use]
    pub fn identity(len: usize) -> Self {
        let mut cells = Self::new();
        cells.push_repeated(0..1, 0, 1, len);
        cells
    }

    /// Whether no cell was recorded.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Where the text of the cell drawn over column `col` starts in the line.
    #[must_use]
    pub fn byte_at_column(&self, col: usize) -> Option<usize> {
        self.runs().find_map(|run| {
            let into = col.checked_sub(run.col)?;
            let width = NonZeroUsize::new(usize::from(run.width))?;
            let i = u32::try_from(into / width)
                .ok()
                .filter(|&i| i < run.count)?;
            Some(run.cell(i).byte)
        })
    }

    /// The bytes of the cells drawn over any of columns `start_col..end_col`.
    #[must_use]
    pub fn bytes_in_columns(&self, start_col: usize, end_col: usize) -> Option<Range<usize>> {
        let mut found: Option<Range<usize>> = None;
        for run in self.runs() {
            let Some(width) = NonZeroUsize::new(usize::from(run.width)) else {
                continue;
            };
            let first = start_col.saturating_sub(run.col) / width;
            let Some(last) = end_col
                .checked_sub(run.col)
                .map_or(0, |into| into.div_ceil(width.get()))
                .min(run.count as usize)
                .checked_sub(1)
                .filter(|&last| last >= first)
            else {
                continue;
            };
            let start = run.cell(u32::try_from(first).ok()?).byte;
            let end = run.cell(u32::try_from(last).ok()?).end;
            found = Some(found.map_or(start..end, |found| {
                found.start.min(start)..found.end.max(end)
            }));
        }
        found
    }

    /// The columns (start, end), end exclusive, that bytes start_byte..end_byte of text.
    #[must_use]
    pub fn columns_of(&self, text: &str, start_byte: usize, end_byte: usize) -> (usize, usize) {
        ColumnMap::with_cells(text, self).span(start_byte, end_byte)
    }

    /// Every cell, in line order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = CellSpan> + '_ {
        self.runs()
            .flat_map(|run| (0..run.count).map(move |i| run.cell(i)))
    }
}

#[cfg(test)]
mod tests {
    use super::{CellSpan, LineCells};

    fn spans(cells: &LineCells) -> Vec<(usize, usize, usize, usize)> {
        cells
            .iter()
            .map(
                |CellSpan {
                     byte,
                     end,
                     col,
                     end_col,
                 }| (byte, end, col, end_col),
            )
            .collect()
    }

    /// back as pushed.
    #[test]
    fn cells_read_back_as_pushed_in_runs_of_one_shape() {
        let mut cells = LineCells::new();
        for i in 0..4 {
            cells.push(i * 3..i * 3 + 3, i * 2, 2);
        }
        assert_eq!(cells.runs().count(), 1, "four ideographs are one run");
        assert!(cells.tail.is_empty(), "held inline");
        cells.push(12..13, 8, 1);
        cells.push(13..14, 10, 1); // a column gap (an orphaned right half)
        cells.push(16..17, 11, 1); // a byte gap (text no cell holds)
        assert_eq!(cells.runs().count(), 4);
        assert_eq!(
            spans(&cells),
            vec![
                (0, 3, 0, 2),
                (3, 6, 2, 4),
                (6, 9, 4, 6),
                (9, 12, 6, 8),
                (12, 13, 8, 9),
                (13, 14, 10, 11),
                (16, 17, 11, 12),
            ]
        );
        assert_eq!(cells.end_col(), 12);
    }

    /// text, is not recorded.
    #[test]
    fn empty_and_overlapping_cells_are_not_recorded() {
        let mut cells = LineCells::new();
        cells.push(0..3, 0, 2);
        cells.push(3..3, 2, 1);
        cells.push(2..4, 2, 1);
        assert_eq!(spans(&cells), vec![(0, 3, 0, 2)]);
    }

    /// a run that continues across the join stays one run.
    #[test]
    fn appended_cells_are_shifted_and_merged() {
        let mut row = LineCells::new();
        row.push(0..1, 0, 1);
        row.push(1..2, 1, 1);
        let mut joined = row.clone();
        joined.append_shifted(&row, 2, 2);
        assert_eq!(joined.runs().count(), 1, "four ASCII cells in a row");
        joined.append_shifted(&row, 4, 10);
        assert_eq!(
            spans(&joined),
            vec![
                (0, 1, 0, 1),
                (1, 2, 1, 2),
                (2, 3, 2, 3),
                (3, 4, 3, 4),
                (4, 5, 10, 11),
                (5, 6, 11, 12),
            ]
        );
    }

    /// head or a continuation, wherever the joins pad or shift its cells.
    #[test]
    fn a_joined_line_stops_short_when_a_row_does() {
        let mut row = LineCells::new();
        row.push(0..1, 0, 1);
        let mut cut = row.clone();
        cut.mark_truncated();
        let mut whole = row.clone();
        whole.append_wrapped_row(&mut "x".to_string(), "y", &row, 4);
        assert!(!whole.is_truncated());
        for (head, continuation) in [(&cut, &row), (&row, &cut)] {
            let mut joined = head.clone();
            let mut line = "x".to_string();
            joined.append_wrapped_row(&mut line, "y", continuation, 4);
            assert!(joined.is_truncated(), "{head:?} + {continuation:?}");
            assert_eq!(line, "x   y", "the stride is unchanged");
        }
    }

    /// not 4, and a short or empty row is padded out with one-column spaces.
    #[test]
    fn a_wrapped_row_joins_at_its_stride() {
        let mut row0 = LineCells::new();
        row0.push(0..1, 0, 1);
        row0.push(1..2, 1, 1);
        row0.push(2..8, 2, 1); // U+2714 U+FE0F, one cell wide
        let mut row1 = LineCells::new();
        row1.push(0..1, 0, 1);
        let mut line = String::from("ab\u{2714}\u{fe0f}");
        let mut joined = row0;
        joined.append_wrapped_row(&mut line, "c", &row1, 3);
        joined.append_wrapped_row(&mut line, "", &LineCells::new(), 6);
        joined.append_wrapped_row(&mut line, "d", &row1, 9);
        assert_eq!(line, "ab\u{2714}\u{fe0f}c     d");
        assert_eq!(
            spans(&joined),
            vec![
                (0, 1, 0, 1),
                (1, 2, 1, 2),
                (2, 8, 2, 3),
                (8, 9, 3, 4),
                (9, 10, 4, 5),
                (10, 11, 5, 6),
                (11, 12, 6, 7),
                (12, 13, 7, 8),
                (13, 14, 8, 9),
                (14, 15, 9, 10),
            ]
        );
    }

    /// and a column no cell covers names none.
    #[test]
    fn a_column_names_the_cell_drawn_over_it() {
        let mut cells = LineCells::new();
        cells.push(0..4, 0, 2);
        cells.push(4..8, 2, 2);
        cells.push(8..9, 5, 1);
        let found: Vec<_> = (0..7).map(|col| cells.byte_at_column(col)).collect();
        assert_eq!(
            found,
            [Some(0), Some(0), Some(4), Some(4), None, Some(8), None]
        );
        let text = "\u{1f1e6}\u{1f1e7}x";
        assert_eq!(cells.columns_of(text, 0, 8), (0, 4));
        assert_eq!(cells.columns_of(text, 8, 9), (5, 6));
    }

    /// cell is drawn.
    #[test]
    fn a_column_range_takes_the_cells_drawn_over_it() {
        let mut cells = LineCells::new();
        cells.push(0..4, 0, 2);
        cells.push(4..8, 2, 2);
        cells.push(8..9, 5, 1);
        cells.push(9..10, 6, 1);
        cells.push_repeated(10..13, 7, 2, 3);
        for (cols, want) in [
            ((0, 1), Some(0..4)),
            ((1, 2), Some(0..4)),
            ((1, 3), Some(0..8)),
            ((3, 4), Some(4..8)),
            ((4, 5), None),
            ((4, 6), Some(8..9)),
            ((5, 7), Some(8..10)),
            ((8, 9), Some(10..13)),
            ((8, 10), Some(10..16)),
            ((6, 100), Some(9..19)),
            ((13, 14), None),
            ((0, 0), None),
        ] {
            assert_eq!(cells.bytes_in_columns(cols.0, cols.1), want, "{cols:?}");
        }
        assert_eq!(LineCells::new().bytes_in_columns(0, 10), None);
    }
}
