// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The grid cells a line of indexed or selected text was read from.

use std::num::NonZeroUsize;
use std::ops::Range;

use crate::grapheme::stored_span;

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

/// `count` cells of one shape: cell `i` holds bytes
/// `byte + i·bytes .. byte + (i+1)·bytes` and is drawn from column
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
    /// Decode the last cell starting at or before `at`, and the cursor before it.
    #[inline(always)]
    fn pair_at_byte(self, at: usize, preceding: usize) -> (Option<CellSpan>, usize) {
        // Recording checked positive byte strides and counts; the caller chose
        // this run by its start byte. Cell decoding retains saturation.
        let i = ((at - self.byte) / self.bytes as usize).min(self.count as usize - 1) as u32;
        let cell = self.cell(i);
        let before = if i > 0 { cell.col } else { preceding };
        (Some(cell), before)
    }

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

    /// Whether `next` continues this run: the same shape, starting where the
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

    /// Whether the text read with these cells stops short of the line's end
    /// ([`Self::mark_truncated`]).
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
        let col = self.end_col();
        let padding = row_start.saturating_sub(col);
        if padding > 0 {
            let byte = line.len();
            line.reserve(padding.saturating_add(text.len()));
            const SPACES: &str = "                                                                ";
            let mut written = padding.min(SPACES.len());
            line.push_str(&SPACES[..written]);
            while written < padding {
                let count = written.min(padding - written);
                line.extend_from_within(byte..byte + count);
                written += count;
            }
            // Preserve push's overlap rejection even if retained cells extend
            // beyond the supplied text: only the accepted suffix forms a run.
            let skip = self.end_byte().saturating_sub(byte).min(padding);
            let mut remaining = padding - skip;
            let mut byte = byte + skip;
            let mut col = col + skip;
            while remaining > 0 {
                let count = remaining.min(u32::MAX as usize);
                self.push_run(CellRun {
                    byte,
                    col,
                    count: count as u32,
                    bytes: 1,
                    width: 1,
                });
                byte += count;
                col += count;
                remaining -= count;
            }
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
        if start_col >= end_col {
            return None;
        }
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
        stored_span(text, self, start_byte, end_byte)
    }

    /// Last cell starting at or before `byte` and the cursor after its predecessor,
    /// restricted to starts below `len`. Byte order is independent of columns.
    #[inline(always)]
    pub(crate) fn cell_and_previous(&self, byte: usize, len: usize) -> (Option<CellSpan>, usize) {
        let Some(at) = len.checked_sub(1).map(|last| byte.min(last)) else {
            return (None, 0);
        };
        let Some(head) = self.head.filter(|head| head.byte <= at) else {
            return (None, 0);
        };
        if self.tail.first().is_none_or(|run| run.byte > at) {
            return head.pair_at_byte(at, 0);
        }
        // Grow from the front before bisecting: early endpoints need not search
        // a long row's whole tail, and late endpoints still skip repeated runs.
        let mut first = 1;
        let mut end = 1;
        while end < self.tail.len() && self.tail[end].byte <= at {
            first = end + 1;
            end = end.saturating_mul(2).saturating_add(1).min(self.tail.len());
        }
        let count = first + self.tail[first..end].partition_point(|run| run.byte <= at);
        let (run, preceding) = match self.tail[..count].split_last() {
            Some((run, earlier)) => {
                let previous = earlier.last().copied().unwrap_or(head);
                (*run, previous.cell(previous.count).col)
            }
            None => (head, 0),
        };
        run.pair_at_byte(at, preceding)
    }

    /// Every cell, in line order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = CellSpan> + '_ {
        self.runs()
            .flat_map(|run| (0..run.count).map(move |i| run.cell(i)))
    }
}

#[cfg(test)]
#[path = "cells_tests.rs"]
mod tests;
