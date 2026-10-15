// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A row as search reads it: its text as written, and the grid cells that
//! text was read from.
//!
//! The search index reports a match in the cells that hold it
//! ([`LineCells`]), not where a walk of the text would put them: the grid
//! folds a character into the cell before it only when that cell is the one
//! before the cursor as the character arrives, so two flag halves written
//! out of order (or brought together by a delete-character) stay two cells,
//! and a VS16 base in a row's last column stays one cell wide. Every search
//! over this terminal reads its rows here: the full index build, its
//! refresh, the budgeted search, and the GUI's own index.
//!
//! A history row's cells are the ones it is drawn in
//! ([`history_line_text_cells`]): a ring-tier row's own cells, read by the
//! walk the ring tier draws it with (`Grid::ring_history_cells`), and a row
//! past the ring the walk of its stored line it is drawn by. The stored walk
//! costs one unit scan per grapheme, about 0.9 us a row of 70 decomposed
//! accents, measured +17% on a full build of such scrollback and within noise
//! on ASCII, agent output and CJK (the walk steps over printable ASCII runs);
//! accepted (ruling 2026-10-11, r15 content-5), a full build runs only on a
//! cold or invalidated index. Reading the ring tier from its cells (ruling
//! 2026-10-11, r16 selection-stride review: the stored walk joined what the
//! ring tier draws apart, two flag halves drawn out of order among them, so
//! find, a double-click and a copy were up to two cells off there) steps over
//! printable ASCII runs too: over 50,000 rows behind a 10,000-row ring, the
//! median of 6 alternated runs of a full build moved -3.4% (ASCII), 0.0%
//! (agent output), -0.6% (CJK) and +0.5% (decomposed accents), within noise.

use std::borrow::Cow;

use aterm_scrollback::Line;

use super::Terminal;
use super::content::visible_row_written;
use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
use crate::grid::{Grid, stored_line_cells};
use crate::search::LineCells;

impl Terminal {
    /// [`written_line_text`](Self::written_line_text) with the grid cells its
    /// text was read from: a visible row's live cells, or the cells a history
    /// row is drawn in ([`history_line_text_cells`]). `None` when `row`
    /// addresses no line.
    #[must_use]
    pub fn written_line_cells(&self, row: i32) -> Option<(String, LineCells)> {
        if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let idx = self
                .grid
                .scrollback_lines()
                .checked_sub(rev_idx.checked_add(1)?)?;
            let line = self.grid.history_line_rev(rev_idx)?;
            return Some(history_row_written(&self.grid, idx, &line));
        }
        let row = u16::try_from(row)
            .ok()
            .filter(|&row| row < self.grid.rows())?;
        Some(visible_row_written(&self.grid, row))
    }
}

/// History line `idx` (0 = oldest) read for search from the dense history
/// walk (`Grid::history_lines_from`): its text and cells, or an empty row for
/// a line the walk could not read, so later rows keep their absolute rows.
pub(crate) fn history_row_for_search(
    grid: &Grid,
    idx: usize,
    line: Option<Cow<'_, Line>>,
) -> (String, LineCells) {
    line.map(|line| history_row_written(grid, idx, &line))
        .unwrap_or_default()
}

/// History line `idx` (`line`, its stored line) as search reads it: its text
/// as written, bounded by [`MAX_SCROLLBACK_LINE_SCAN_BYTES`] as
/// [`Terminal::written_line_text`] reads it, with the cells that text is
/// drawn in.
fn history_row_written(grid: &Grid, idx: usize, line: &Line) -> (String, LineCells) {
    let text = line_text_bounded(line.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned();
    let cells = history_line_text_cells(grid, idx, line, &text);
    (text, cells)
}

/// The cells `text`, a read of history line `idx` (0 = oldest) of `grid`
/// whose stored line is `line`, is drawn in: the ring tier's own cells where
/// it draws the row from them ([`Grid::ring_history_cells`]), else the walk
/// the row is drawn by from its stored line ([`stored_line_cells`]). Every
/// reader of a history row's cells comes here: search (the index, its
/// refresh, the budgeted search, the GUI's find), a double-click's window,
/// a web host's word under a cell and a copy's columns, so a column names
/// the cell drawn there on either tier (r16 selection-stride review,
/// 2026-10-11: reading every history row from its stored line put words,
/// copies and matches after two flag halves drawn apart two cells off where
/// the ring tier draws them).
#[must_use]
pub fn history_line_text_cells(grid: &Grid, idx: usize, line: &Line, text: &str) -> LineCells {
    let mut cells = LineCells::new();
    if grid.ring_history_cells(idx, text, |bytes, col, width, count| {
        cells.push_repeated(
            bytes,
            usize::from(col),
            usize::from(width),
            usize::from(count),
        );
    }) {
        return cells;
    }
    stored_line_text_cells(line, text, grid.cols())
}

/// The cells `text`, a read of stored history `line`, is drawn in on a row
/// `cols` wide when it is drawn from that line: the walk
/// ([`stored_line_cells`]) a row past the ring tier is laid out by.
pub(crate) fn stored_line_text_cells(line: &Line, text: &str, cols: u16) -> LineCells {
    let mut cells = LineCells::new();
    for (bytes, col, width, count) in stored_line_cells(line, text, cols) {
        cells.push_repeated(
            bytes,
            usize::from(col),
            usize::from(width),
            usize::from(count),
        );
    }
    cells
}
