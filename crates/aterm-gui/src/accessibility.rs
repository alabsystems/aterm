// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.
//!
//! A pure conversion from the rendered cells and cursor into what an assistive
//! backend needs: the visible screen as plain text, a role, a label, the grid size
//! and the cursor's character offset. It holds no GUI state and calls no platform
//! API; the App includes it in the shared semantic tree while an assistive client
//! is attached. The text goes through
//! [`push_visible_row`], as the SIGUSR1 `.txt` snapshot does, so a screen reader and
//! the text snapshot read the same visible-cell projection.

use aterm_core::terminal::RenderCell;

/// Human-facing label announced for the terminal view.
#[cfg(a11y_tree)]
pub(crate) const LABEL: &str = "aterm terminal";

/// Append one grid row to `text`: each cell's glyph char (control/NUL → space),
/// up to `cols`, trailing blanks trimmed, terminated by `'\n'`. Shared by the
/// accessibility snapshot and the SIGUSR1 `.txt` snapshot, so the two are identical.
///
/// One character per cell, wide glyphs included: the engine pads a double-width
/// glyph's continuation cell with a space (`aterm_core::terminal::render_cells`),
/// so `漢 字` reads as separate words. The pad preserves cell-column offsets
/// within each emitted row, before trailing blanks are trimmed.
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

/// Rendered rows as a snapshot reads them: row `r`'s cells over the columns
/// `cols` — fewer for a short row, none for a row past the end. The rendered
/// rows themselves are one source; the presented frame read as its programs
/// wrote it, under aterm's glass, is another
/// (`claude_footer::ProgramCells`).
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

/// A plain-text + cursor snapshot of the visible grid for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(a11y_tree, test))]
pub(crate) struct AccessibleSnapshot {
    /// The visible screen as text: one line per row (trailing blanks trimmed),
    /// rows separated and terminated by `'\n'`.
    pub text: String,
    /// Grid dimensions.
    pub rows: usize,
    /// Grid width in columns.
    pub cols: usize,
    /// Cursor position as `(row, col)`, 0-based, if the cursor is visible.
    pub cursor: Option<(usize, usize)>,
}

#[cfg(any(a11y_tree, test))]
impl AccessibleSnapshot {
    /// Build a snapshot from rendered rows of cells (the tests' engine grids; the
    /// App reads its frame through [`Self::from_cells_in`]).
    ///
    /// `cursor` is `Some((row, col))` only when the cursor is visible; pass `None`
    /// to omit it (e.g. when hidden via DECTCEM).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_cells(
        rows_cells: &[Vec<RenderCell>],
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        Self::from_cells_in(rows_cells, 0..rows_cells.len(), 0..cols, cursor)
    }

    /// Build a snapshot of one rectangle of the rendered rows: the projection a
    /// split pane needs. A composed split frame carries every pane side by side
    /// on the same rows, so a whole row is two programs' output spliced together;
    /// read through one pane's rectangle of the same cells, it is that pane's
    /// painted line.
    ///
    /// `cursor` is pane-local, `None` when the pane's cursor is off screen. Cells
    /// outside the supplied rows are absent, not padded: a short row yields a
    /// short line.
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

    /// The accessibility value: the visible screen text (an `AX` value string).
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    /// A pane's rectangle reads only its own columns of the composed split frame.
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
