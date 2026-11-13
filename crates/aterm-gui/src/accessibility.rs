// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.

use aterm_core::terminal::RenderCell;

/// Human-facing label announced for the terminal view.
#[cfg(a11y_tree)]
pub(crate) const LABEL: &str = "aterm terminal";

/// Refillable extraction storage for the terminal documents read in one window.
/// A completed pass retains only its geometries, without image payloads.
#[cfg(a11y_tree)]
#[derive(Default)]
pub(crate) struct DocumentFrames {
    entries: std::collections::HashMap<(usize, usize), DocumentFrame>,
    epoch: u64,
}

#[cfg(a11y_tree)]
struct DocumentFrame {
    input: aterm_core::terminal::TextFrame,
    epoch: u64,
}

#[cfg(a11y_tree)]
impl DocumentFrames {
    pub(crate) fn pass(&mut self) -> DocumentFramePass<'_> {
        self.epoch = self.epoch.wrapping_add(1);
        DocumentFramePass { frames: self }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Exclusive storage for a synchronous document read; its drop retires unused frames.
#[cfg(a11y_tree)]
pub(crate) struct DocumentFramePass<'a> {
    frames: &'a mut DocumentFrames,
}

#[cfg(a11y_tree)]
impl DocumentFramePass<'_> {
    pub(crate) fn frame(
        &mut self,
        rows: usize,
        cols: usize,
    ) -> &mut aterm_core::terminal::TextFrame {
        let epoch = self.frames.epoch;
        let entry = self
            .frames
            .entries
            .entry((rows, cols))
            .or_insert_with(|| DocumentFrame {
                input: aterm_core::terminal::TextFrame::default(),
                epoch,
            });
        entry.epoch = epoch;
        &mut entry.input
    }
}

#[cfg(a11y_tree)]
impl Drop for DocumentFramePass<'_> {
    fn drop(&mut self) {
        let epoch = self.frames.epoch;
        self.frames.entries.retain(|_, entry| entry.epoch == epoch);
    }
}

/// Appends one grid row to `text` up to `cols`, trimming trailing spaces and ending with `'\n'`.
/// Each cell contributes its base character alone, so the combining marks and joined
/// characters of a multi-scalar cluster are left out.
pub(crate) fn push_visible_row<'a>(
    text: &mut String,
    cells: impl IntoIterator<Item = &'a RenderCell>,
    cols: usize,
) {
    for cell in cells.into_iter().take(cols) {
        text.push(if cell.ch == '\0' || cell.ch.is_control() {
            ' '
        } else {
            cell.ch
        });
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text.push('\n');
}

/// A plain-text and cursor snapshot of the visible grid for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(a11y_tree, test))]
pub(crate) struct AccessibleSnapshot {
    /// Visible screen text, one newline-terminated line per row with trailing blanks trimmed.
    pub text: String,
    /// Grid height in rows.
    pub rows: usize,
    /// Grid width in columns.
    pub cols: usize,
    /// Cursor position as 0-based `(row, col)` when visible.
    pub cursor: Option<(usize, usize)>,
}

#[cfg(any(a11y_tree, test))]
impl AccessibleSnapshot {
    /// Builds a snapshot from rendered rows of cells.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_cells(
        rows_cells: &[Vec<RenderCell>],
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        Self::from_cells_in(rows_cells, 0..rows_cells.len(), 0..cols, cursor)
    }

    /// Builds a snapshot of the rectangle `rows` × `cols` within `rows_cells`.
    #[must_use]
    pub(crate) fn from_cells_in(
        rows_cells: &[Vec<RenderCell>],
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        let width = cols.len();
        let mut text = String::with_capacity(rows.len() * (width + 1));
        for r in rows.clone() {
            let row = rows_cells.get(r).map_or(&[][..], Vec::as_slice);
            let end = cols.end.max(cols.start).min(row.len());
            push_visible_row(&mut text, &row[cols.start.min(row.len())..end], width);
        }
        Self {
            text,
            rows: rows.len(),
            cols: width,
            cursor,
        }
    }

    /// Returns the visible screen text as an accessibility value string.
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
#[path = "accessibility_tests.rs"]
mod tests;
