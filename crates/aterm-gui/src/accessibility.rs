// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.

use aterm_core::terminal::RenderCell;

/// Human-facing label announced for the terminal view.
#[cfg(a11y_tree)]
pub(crate) const LABEL: &str = "aterm terminal";

/// Appends one grid row to `text` up to `cols`, trimming trailing spaces and ending with `'\n'`.
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

/// Rendered rows read by an accessibility snapshot.
#[cfg(any(a11y_tree, test))]
pub(crate) trait CellRows {
    /// Row `r`'s cells in `cols`, left to right.
    fn cells_in(&self, r: usize, cols: std::ops::Range<usize>)
    -> impl Iterator<Item = &RenderCell>;
}

#[cfg(any(a11y_tree, test))]
impl CellRows for [Vec<RenderCell>] {
    fn cells_in(
        &self,
        r: usize,
        cols: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &RenderCell> {
        let row: &[RenderCell] = self.get(r).map_or(&[], Vec::as_slice);
        let end = cols.end.max(cols.start).min(row.len());
        row[cols.start.min(row.len())..end].iter()
    }
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
        rows_cells: &(impl CellRows + ?Sized),
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        let width = cols.len();
        let mut text = String::with_capacity(rows.len() * (width + 1));
        for r in rows.clone() {
            push_visible_row(&mut text, rows_cells.cells_in(r, cols.clone()), width);
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
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    #[test]
    fn a_pane_rectangle_reads_only_its_own_columns() {
        let mut term = Terminal::new(2, 21);
        term.process(b"left\x1b[11G\xe2\x96\x90right");
        let cells: Vec<Vec<RenderCell>> = (0..2).map(|r| term.render_row(r)).collect();

        let whole = AccessibleSnapshot::from_cells(&cells, 21, None);
        let composed = whole.value().lines().next().unwrap();
        assert!(
            composed.contains("left") && composed.contains("right"),
            "the composed row carries both panes: {composed:?}"
        );

        let left = AccessibleSnapshot::from_cells_in(&cells[..], 0..2, 0..10, None);
        let right = AccessibleSnapshot::from_cells_in(&cells[..], 0..2, 11..21, Some((0, 5)));
        assert_eq!(left.value(), "left\n\n", "no divider, no neighbour");
        assert_eq!(right.value(), "right\n\n");
        assert_eq!((left.rows, left.cols), (2, 10));
        assert_eq!(right.cursor, Some((0, 5)), "the pane-local caret");
    }
}
