// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text selection to string conversion for `Terminal`: `selection_to_string()`,
//! `get_line_text()` and the logical-line helpers the click gestures use.

use std::borrow::Cow;
use std::ops::Range;

use super::Terminal;
use super::content::{TrailingBlanks, visible_row_bounds_to_string};
use crate::search::LineCells;
use crate::selection::SmartSelection;

/// Cap on the rows [`Terminal::selection_to_string`] materializes in one call: retention
/// can be unlimited, and a control-socket select-all plus copy would otherwise do
/// O(scrollback) work under the terminal mutex. Ten times the default 100k-line retention.
const MAX_SELECTION_ROWS: usize = 1_000_000;

/// Cap on the bytes [`Terminal::selection_to_string`] produces, the binding memory bound:
/// one stored line can be `MAX_GRID_COLS` (4096) clusters of multi-byte text. 64 MiB
/// exceeds a full default 100k-line scrollback (~20 MiB).
const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;

/// Cap, each way, on the physical rows [`Terminal::logical_line_span`] walks to join one
/// soft-wrapped line: a history row's wrap bit can cost a tier fetch under the terminal
/// mutex, and scrollback can be unlimited. 4096 rows never clips a line a human selects.
const MAX_LOGICAL_LINE_ROWS: usize = 4096;

/// Per-line byte ceiling for scrollback text extraction. A scrollback
/// [`Line`](aterm_scrollback::Line) has no length bound (a crafted checkpoint can inject a
/// multi-MiB one), so extraction bounds both the copy and the grapheme walk. A real line
/// is at most `MAX_GRID_COLS` (4096) cells of 1 base + `Extra::MAX_COMBINING` (16) marks
/// (~272 KiB), so 1 MiB never clips one.
pub(crate) const MAX_SCROLLBACK_LINE_SCAN_BYTES: usize = 1024 * 1024;

/// A stored line's full text (all columns, no trim) bounded to `max_scan_bytes`: a
/// [`Line::to_string`](aterm_scrollback::Line) an oversized line cannot blow up. Borrows
/// valid UTF-8 under the ceiling; lossy-converts only a bounded prefix otherwise.
pub(crate) fn line_text_bounded(bytes: &[u8], max_scan_bytes: usize) -> Cow<'_, str> {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    String::from_utf8_lossy(scan)
}

/// Extract a column range from a scrollback line's raw bytes without materializing the
/// whole line: the bytes of the cells drawn over columns `start_col..=end_col`, `cells_of`
/// giving the cells the line's bounded text is drawn in ([`history_line_text_cells`]). A
/// cell the range covers only half of (a wide glyph straddling an edge) is copied whole,
/// the scrollback twin of [`visible_row_bounds_to_string`]'s
/// [`glyph_cell_span`](aterm_types::selection::glyph_cell_span) edges, so a stored line
/// copies the cells the highlight paints. The columns are the cells', never a walk of the
/// text under the `ambiguous_width` policy now in force: a row keeps the cells it was drawn
/// in. The text and its cell walk are bounded by `max_scan_bytes`
/// ([`MAX_SCROLLBACK_LINE_SCAN_BYTES`] in production; a parameter so tests can use a small
/// one); columns past the scanned content yield an empty string. A stored line holds
/// exactly the cells its row had written, so [`TrailingBlanks::Written`] keeps the range as
/// stored.
///
/// [`history_line_text_cells`]: super::history_line_text_cells
/// [`visible_row_bounds_to_string`]: super::content::visible_row_bounds_to_string
fn scrollback_line_range_text(
    bytes: &[u8],
    max_scan_bytes: usize,
    cells_of: impl FnOnce(&str) -> LineCells,
    (start_col, end_col): (u16, u16),
    trailing: TrailingBlanks,
) -> String {
    let line = line_text_bounded(bytes, max_scan_bytes);
    let cells = cells_of(&line);
    let Some(range) = cells
        .bytes_in_columns(
            usize::from(start_col),
            usize::from(end_col).saturating_add(1),
        )
        .and_then(|bytes| line.get(bytes))
    else {
        return String::new();
    };
    match trailing {
        TrailingBlanks::Trim => range.trim_end().to_string(),
        TrailingBlanks::Written => range.to_string(),
    }
}

/// Append `row_text` to `result` without exceeding `max_bytes`, returning
/// `true` if the row was clamped (only a char-boundary-safe prefix was pushed).
/// Makes [`MAX_SELECTION_BYTES`] a hard cap even for one oversized row (up to
/// `MAX_GRID_COLS` * `MAX_GRAPHEME_UNIT_BYTES`, ~1 MiB).
fn push_row_capped(result: &mut String, row_text: &str, max_bytes: usize) -> bool {
    if result.len().saturating_add(row_text.len()) <= max_bytes {
        result.push_str(row_text);
        return false;
    }
    let budget = max_bytes.saturating_sub(result.len());
    let mut cut = budget.min(row_text.len());
    while cut > 0 && !row_text.is_char_boundary(cut) {
        cut -= 1;
    }
    result.push_str(&row_text[..cut]);
    true
}

/// The row/column span one selection walk needs, computed once by
/// [`Terminal::selection_to_string_capped`] and passed to whichever shape walk
/// runs, so the walks cannot drift apart in geometry.
#[derive(Clone, Copy)]
struct SelectionGeometry {
    /// Oldest terminal-relative row to visit (span-clamped to `-scrollback_lines`).
    first_row: i32,
    /// Newest terminal-relative row to visit (span-clamped to `rows - 1`).
    last_row: i32,
    /// Side-adjusted selection start row — the row whose text starts at `adj_start_col`.
    adj_start_row: i32,
    /// Side-adjusted selection end row — the row whose text stops at `adj_end_col`.
    adj_end_row: i32,
    /// Side-adjusted start column, inclusive.
    adj_start_col: u16,
    /// Side-adjusted end column, inclusive.
    adj_end_col: u16,
    /// `grid.cols()`; the caller has already rejected a zero-column grid.
    cols: u16,
}

/// One row of a linear selection walk, read once (see
/// [`Terminal::push_linear_selection`]).
struct SelectionRow<'a> {
    /// The stored line of a history row; `None` for a live row, or a history
    /// row that could not be read.
    history: Option<Cow<'a, aterm_scrollback::Line>>,
    /// Whether the row continues the one above (a soft wrap).
    continues: bool,
}

/// The word `smart`'s rules find at column `col` of `text`, read with the grid cells it is
/// drawn in (`cells`, as [`Terminal::written_line_cells`] and
/// [`Terminal::line_range_cells`] read them): its bytes in `text` and the columns
/// `(start, end)`, `end` exclusive, it is drawn in. `None` on a column no cell is drawn
/// over (right of the text) or on whitespace.
///
/// Both directions go through the cells ([`LineCells::byte_at_column`],
/// [`LineCells::columns_of`]), never a walk of the text, which does not know the cells the
/// grid kept apart or narrow (two flag halves drawn out of order, a VS16 base in the last
/// column).
#[must_use]
pub fn smart_word_in_cells(
    smart: &SmartSelection,
    text: &str,
    cells: &LineCells,
    col: usize,
) -> Option<(Range<usize>, (usize, usize))> {
    let byte = cells.byte_at_column(col)?;
    let (start, end) = smart.word_boundaries_at(text, byte)?;
    Some((start..end, cells.columns_of(text, start, end)))
}

impl Terminal {
    /// The selected text; `None` if there is no selection or it is empty. Block selections
    /// separate rows with newlines. Bounded by [`MAX_SELECTION_ROWS`] /
    /// [`MAX_SELECTION_BYTES`], dropping clipped content silently; see
    /// [`Self::selection_to_string_bounded`].
    #[must_use]
    pub fn selection_to_string(&self) -> Option<String> {
        self.selection_to_string_bounded().0
    }

    /// [`Self::selection_to_string`] and whether selected content is missing from either
    /// end (the copy caps drop the tail; a scrollback eviction that clamped the head drops
    /// the front), so callers can report it as search reports `incomplete`.
    #[must_use]
    pub fn selection_to_string_bounded(&self) -> (Option<String>, bool) {
        self.selection_to_string_capped(MAX_SELECTION_ROWS, MAX_SELECTION_BYTES)
    }

    /// [`Self::selection_to_string`] with explicit work/output caps, returning
    /// the text and a `truncated` flag; split out so tests can use small caps.
    fn selection_to_string_capped(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> (Option<String>, bool) {
        use crate::selection::SelectionType;

        // Side-adjusted bounds, so the copy matches the highlight (a Right-sided start or
        // Left-sided end excludes its character). Eviction is read first: a clamped head
        // can collapse the span, and that still reports incomplete, not an empty `OK 0`.
        let evicted = self.text_selection.truncated();
        let Some((adj_start_row, adj_start_col, adj_end_row, adj_end_col)) =
            self.text_selection.side_adjusted_bounds()
        else {
            return (None, evicted);
        };

        let mut result = String::new();
        let cols = self.grid.cols();
        if cols == 0 {
            // A degenerate geometry, not a partial report about a span that exists.
            return (None, false);
        }

        // Two bounds keep a selection from spinning or allocating unboundedly under the
        // terminal mutex: the span is clamped to [-scrollback_lines, rows-1] (control-socket
        // anchors are unclamped i32s), and MAX_SELECTION_ROWS / MAX_SELECTION_BYTES bound
        // the walks (content itself can be unbounded).
        let visible_rows = i32::from(self.grid.rows());
        let history = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        let first_row = adj_start_row.max(-history);
        let last_row = adj_end_row.min(visible_rows - 1);

        let geom = SelectionGeometry {
            first_row,
            last_row,
            adj_start_row,
            adj_end_row,
            adj_start_col,
            adj_end_col,
            cols,
        };

        // Oldest to newest, so a cap truncates the tail and the start is always kept.
        let (rows_emitted, truncated) = match self.text_selection.selection_type() {
            SelectionType::Block => {
                self.push_block_selection(&mut result, geom, max_rows, max_bytes)
            }
            // Simple, Semantic, Lines, and future variants all use linear selection
            _ => self.push_linear_selection(&mut result, geom, max_rows, max_bytes),
        };

        if truncated {
            // Logged for callers that take only the text (mouse, wasm); `cmd_selection`
            // and `cmd_copy` append ` incomplete` from the returned flag.
            aterm_log::warn!(
                "selection_to_string truncated at cap (rows_emitted={rows_emitted}, bytes={})",
                result.len()
            );
        }

        // The two halves are independent (a capped walk over a clamped selection misses
        // text at both ends), so they or.
        if result.is_empty() {
            (None, truncated || evicted)
        } else {
            (Some(result), truncated || evicted)
        }
    }

    /// Walk a block (rectangular) selection into `result`, returning
    /// `(rows_emitted, truncated)`. `geom` carries the span the caller computed.
    fn push_block_selection(
        &self,
        result: &mut String,
        geom: SelectionGeometry,
        max_rows: usize,
        max_bytes: usize,
    ) -> (usize, bool) {
        let SelectionGeometry {
            first_row,
            last_row,
            adj_start_col,
            adj_end_col,
            ..
        } = geom;
        let mut rows_emitted = 0usize;
        let mut truncated = false;

        for row in first_row..=last_row {
            if rows_emitted >= max_rows || result.len() >= max_bytes {
                truncated = true;
                break;
            }
            rows_emitted += 1;
            if row > first_row {
                result.push('\n');
            }
            if let Some(line) = self.get_line_text(row, Some((adj_start_col, adj_end_col))) {
                if push_row_capped(result, &line, max_bytes) {
                    truncated = true;
                    break;
                }
            }
        }

        (rows_emitted, truncated)
    }

    /// Walk a linear (Simple/Semantic/Lines) selection into `result`, returning
    /// `(rows_emitted, truncated)`, with one tier fetch per history row.
    ///
    /// Trailing blanks are trimmed where a logical line ends (a hard break or the
    /// selection's last row), from the whole line: a row the next row continues keeps
    /// every cell the program wrote, so a space printed in the last column joins the two
    /// words it separates. The cell a wide glyph leaves when it wraps early was never
    /// written, so it is not in the row's text either.
    fn push_linear_selection(
        &self,
        result: &mut String,
        geom: SelectionGeometry,
        max_rows: usize,
        max_bytes: usize,
    ) -> (usize, bool) {
        let SelectionGeometry {
            first_row,
            last_row,
            adj_start_row,
            adj_end_row,
            adj_start_col,
            adj_end_col,
            cols,
        } = geom;
        let mut rows_emitted = 0usize;
        let mut truncated = false;
        // A row's trailing blanks are content when the next row continues it, so the next
        // row is read one iteration early and carried over: each history row is still
        // fetched from the tier once (a fetch builds or clones a full `Line`).
        let mut lookahead: Option<SelectionRow<'_>> = None;
        // Where the current logical line starts in `result`; its end trims back to here.
        let mut line_start = result.len();

        for row in first_row..=last_row {
            if rows_emitted >= max_rows || result.len() >= max_bytes {
                truncated = true;
                break;
            }
            rows_emitted += 1;
            let current = lookahead.take().unwrap_or_else(|| self.selection_row(row));
            // A newline only before a row that does not continue the one above.
            if row > first_row && !current.continues {
                result.push('\n');
                line_start = result.len();
            }

            let start_col = if row == adj_start_row {
                adj_start_col
            } else {
                0
            };
            let end_col = if row == adj_end_row {
                adj_end_col
            } else {
                cols - 1
            };
            // Only the end row stops short of the row's last column.
            let trailing = if row < last_row {
                let next = self.selection_row(row + 1);
                let continued = next.continues;
                lookahead = Some(next);
                if continued {
                    TrailingBlanks::Written
                } else {
                    TrailingBlanks::Trim
                }
            } else {
                TrailingBlanks::Trim
            };

            // The history arm is `get_line_text`'s on the line already fetched.
            let line = if row < 0 {
                current.history.as_ref().and_then(|stored| {
                    self.history_range_text(row, stored, (start_col, end_col), trailing)
                })
            } else {
                self.line_text(row, Some((start_col, end_col)), trailing)
            };
            if let Some(line) = line {
                if push_row_capped(result, &line, max_bytes) {
                    truncated = true;
                    break;
                }
            }
            // The logical line ends here: trim it whole, so blanks a wrapped row kept go
            // when nothing but blanks follows them.
            if trailing == TrailingBlanks::Trim {
                let keep = line_start + result[line_start..].trim_end().len();
                result.truncate(keep);
            }
        }

        (rows_emitted, truncated)
    }

    /// Read selection-space `row` for [`Self::push_linear_selection`]: its stored line when
    /// history (fetched once, here) and whether it continues the row above.
    fn selection_row(&self, row: i32) -> SelectionRow<'_> {
        let history = if row < 0 {
            usize::try_from(-(i64::from(row)) - 1)
                .ok()
                .and_then(|rev_idx| self.grid.history_line_rev(rev_idx))
        } else {
            None
        };
        let continues = self.continuation_bit(row, history.as_deref());
        SelectionRow { history, continues }
    }

    /// Whether selection-space `row` continues the previous row (a soft wrap). The one place
    /// selection reads the wrap bit: [`Self::push_linear_selection`] (newlines in the copy)
    /// and [`Self::logical_line_span`] (click reach) must agree, or highlight and clipboard
    /// describe different text. `history_line` is the caller's already-fetched line for a
    /// negative row; `None` fetches it.
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "private row/line types prevent method-reference shorthand"
    )]
    fn continuation_bit(&self, row: i32, history_line: Option<&aterm_scrollback::Line>) -> bool {
        let visible_rows = i32::from(self.grid.rows());
        if row >= 0 && row < visible_rows {
            u16::try_from(row)
                .ok()
                .and_then(|idx| self.grid.row_at_screen(idx))
                .is_some_and(|r| r.is_wrapped())
        } else if row < 0 {
            match history_line {
                Some(line) => line.is_wrapped(),
                None => usize::try_from(-(i64::from(row)) - 1)
                    .ok()
                    .and_then(|rev_idx| self.grid.history_line_rev(rev_idx))
                    .is_some_and(|l| l.is_wrapped()),
            }
        } else {
            false
        }
    }

    /// Whether selection-space `row` continues the previous row (soft wrap);
    /// the self-resolving form of [`Self::continuation_bit`].
    #[must_use]
    pub fn row_continues_previous(&self, row: i32) -> bool {
        self.continuation_bit(row, None)
    }

    /// The inclusive physical row span `(first, last)` of the logical line containing
    /// selection-space `row`: the maximal run joined by soft wraps, what word/line click
    /// gestures select, matching the copy walk ([`Self::push_linear_selection`]). The walk
    /// stops at a hard newline, the live screen's bottom and the scrollback floor (as
    /// `adjust_for_scroll` clamps), bounded by [`MAX_LOGICAL_LINE_ROWS`] each way; an
    /// unwrapped or out-of-range row answers `(row, row)`.
    #[must_use]
    pub fn logical_line_span(&self, row: i32) -> (i32, i32) {
        let visible_rows = i32::from(self.grid.rows());
        let floor = -i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        if row < floor || row >= visible_rows {
            return (row, row);
        }
        let cap = i32::try_from(MAX_LOGICAL_LINE_ROWS).unwrap_or(i32::MAX);
        let up_stop = row.saturating_sub(cap).max(floor);
        let mut first = row;
        while first > up_stop && self.row_continues_previous(first) {
            first -= 1;
        }
        let down_stop = row.saturating_add(cap).min(visible_rows - 1);
        let mut last = row;
        while last < down_stop && self.row_continues_previous(last + 1) {
            last += 1;
        }
        (first, last)
    }

    /// Rows `first..=last` of a logical line (already clamped to its span) joined into one
    /// line, with the grid cells each byte was read from: `(text, cells)`. Row `k` is drawn
    /// from column `k * cols` of the join, so column `g` is cell
    /// `(first + g / cols, g % cols)`. Each row is read with its cells
    /// ([`Self::written_line_cells`]), cut after its last cell ([`Self::drawn_line_cells`]),
    /// and joined by [`LineCells::append_wrapped_row`], which pads a short row to its width
    /// with spaces that read as a break. No column is counted by re-measuring a row's text:
    /// a VS16 base kept one cell wide or two flag halves drawn apart measure otherwise.
    /// `None` only when `first` addresses no line.
    ///
    /// Word rules scan only this window, widening it when a match runs to an edge, so a
    /// double-click on a very long logical line does not scan the whole join under the
    /// terminal mutex.
    #[must_use]
    pub fn line_range_cells(&self, first: i32, last: i32) -> Option<(String, LineCells)> {
        let (mut text, mut cells) = self.drawn_line_cells(first)?;
        let cols = usize::from(self.grid.cols());
        let mut row_start = 0usize;
        for r in first.saturating_add(1)..=last {
            row_start = row_start.saturating_add(cols);
            let (row, row_cells) = self.drawn_line_cells(r).unwrap_or_default();
            cells.append_wrapped_row(&mut text, &row, &row_cells, row_start);
        }
        Some((text, cells))
    }

    /// [`Self::written_line_cells`] cut after the row's last cell: the text the row shows.
    /// A stored line can hold text past the grid's width that no cell draws (restored from
    /// a checkpoint written wider); cutting it keeps a double-click's window, scanned
    /// under the terminal mutex, as long as the rows it spans.
    pub(crate) fn drawn_line_cells(&self, row: i32) -> Option<(String, LineCells)> {
        let (mut text, cells) = self.written_line_cells(row)?;
        let end = cells.end_byte();
        if text.is_char_boundary(end) {
            text.truncate(end);
        }
        Some((text, cells))
    }

    /// A line's text (visible or scrollback), trailing blanks trimmed. `row` is
    /// terminal-relative and scroll-invariant: `0..rows` are the live screen rows,
    /// negative rows are scrollback (`-1` = newest). `col_range` is inclusive; `None` is
    /// the whole line.
    pub fn get_line_text(&self, row: i32, col_range: Option<(u16, u16)>) -> Option<String> {
        self.line_text(row, col_range, TrailingBlanks::Trim)
    }

    /// The text a row holds as written, trailing blanks included: every cell a live row's
    /// program wrote (up to `Row::len`), or a history row's stored line whole (bounded by
    /// [`MAX_SCROLLBACK_LINE_SCAN_BYTES`]). One extractor for both, so what search indexes
    /// does not change when output scrolls the row off. `None` when `row` addresses no
    /// line.
    #[must_use]
    pub fn written_line_text(&self, row: i32) -> Option<String> {
        if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let line = self.grid.history_line_rev(rev_idx)?;
            return Some(
                line_text_bounded(line.as_bytes(), MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned(),
            );
        }
        self.line_text(row, None, TrailingBlanks::Written)
    }

    /// [`Self::get_line_text`] with the trailing-blank rule chosen.
    fn line_text(
        &self,
        row: i32,
        col_range: Option<(u16, u16)>,
        trailing: TrailingBlanks,
    ) -> Option<String> {
        let visible_rows = i32::from(self.grid.rows());

        if row >= 0 && row < visible_rows {
            let row_idx = u16::try_from(row).ok()?;
            let cols = self.grid.cols();
            if cols == 0 && col_range.is_none() {
                return Some(String::new());
            }
            let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));
            Some(visible_row_bounds_to_string(
                &self.grid, row_idx, start_col, end_col, trailing,
            ))
        } else if row < 0 {
            // rev_idx 0 is the most recently scrolled-off line (ring or tiered).
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let scrollback_line = self.grid.history_line_rev(rev_idx)?;
            let cols = self.grid.cols();
            if cols == 0 && col_range.is_none() {
                return Some(String::new());
            }
            let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));
            self.history_range_text(row, &scrollback_line, (start_col, end_col), trailing)
        } else {
            None
        }
    }
}

impl Terminal {
    /// The text in columns `start_col..=end_col` of history row `row` (selection-space,
    /// negative; `line` its stored line), read from the cells the row is drawn in. `None`
    /// when `row` addresses no history line.
    fn history_range_text(
        &self,
        row: i32,
        line: &aterm_scrollback::Line,
        cols: (u16, u16),
        trailing: TrailingBlanks,
    ) -> Option<String> {
        let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
        let idx = self
            .grid
            .scrollback_lines()
            .checked_sub(rev_idx.checked_add(1)?)?;
        Some(scrollback_line_range_text(
            line.as_bytes(),
            MAX_SCROLLBACK_LINE_SCAN_BYTES,
            |text| super::history_line_text_cells(&self.grid, idx, line, text),
            cols,
            trailing,
        ))
    }
}

#[cfg(test)]
mod tests {

    /// Each selected history row is fetched from the tier once: `take_row_to_line_ops`
    /// counts `Line` constructions, and an N-row selection costs N.
    #[test]
    fn scrollback_selection_reads_each_history_line_once() {
        use crate::selection::{SelectionSide, SelectionType};
        use crate::terminal::Terminal;

        const ROWS: u16 = 4;
        const COLS: u16 = 16;
        const FILL: usize = 60;
        const SELECTED: usize = 40;

        let mut term = Terminal::new(ROWS, COLS);
        let mut corpus = String::new();
        for i in 0..FILL {
            corpus.push_str(&format!("row{i:03}\r\n"));
        }
        term.process(corpus.as_bytes());
        // Not counting the fixture's own ring reads and writes.
        let _ = aterm_grid::test_counters::take_row_to_line_ops();

        let selected_rows = i32::try_from(SELECTED).expect("SELECTED fits i32");
        let sel = term.text_selection_mut();
        sel.start_selection(
            -selected_rows,
            0,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        sel.update_selection(-1, COLS - 1, SelectionSide::Right);
        sel.complete_selection();

        let text = term
            .selection_to_string()
            .expect("a 40-row scrollback selection copies text");
        let reads = aterm_grid::test_counters::take_row_to_line_ops();

        // History holds fill lines 0..=(FILL - ROWS): row -1 is fill line FILL - ROWS.
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            SELECTED,
            "a {SELECTED}-row hard-broken selection must copy {SELECTED} lines"
        );
        let newest = FILL - usize::from(ROWS);
        assert_eq!(
            lines[SELECTED - 1].trim_end(),
            format!("row{newest:03}"),
            "the last copied line is not the newest selected history row"
        );
        assert_eq!(
            lines[0].trim_end(),
            format!("row{:03}", newest + 1 - SELECTED),
            "the first copied line is not the oldest selected history row"
        );

        assert_eq!(
            reads, SELECTED,
            "a {SELECTED}-row scrollback selection performed {reads} tier line \
             constructions; one per selected row is the whole point of the hoist \
             (the double fetch cost 2N-1)"
        );
    }

    /// Select rows `first..=last` whole, as a drag does, and copy.
    fn copy_rows(term: &mut crate::terminal::Terminal, first: i32, last: i32) -> Option<String> {
        use crate::selection::{SelectionSide, SelectionType};
        let cols = term.cols();
        let sel = term.text_selection_mut();
        sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(last, cols - 1, SelectionSide::Right);
        sel.complete_selection();
        term.selection_to_string()
    }

    #[test]
    fn line_selection_copies_every_highlighted_row_before_column_expansion() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(3, 10);
        term.process(b"first row\r\nsecond row");
        let selection = term.text_selection_mut();
        selection.start_selection(0, 5, SelectionSide::Right, SelectionType::Lines);
        selection.update_selection(1, 0, SelectionSide::Left);
        selection.complete_selection();
        assert!(
            selection.contains(1, 0),
            "the endpoint row is highlighted whole"
        );
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("first row\nsecond row")
        );
    }

    /// A space printed in a wrapped row's last column is kept at the join (screen, history,
    /// triple-click); a row that ends its line still loses its trailing blanks.
    #[test]
    fn copy_keeps_a_space_printed_in_a_wrapped_rows_last_column() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(4, 10);
        // The space lands in column 9 and `jklmn` soft-wraps onto row 1.
        term.process(b"abcdefghi jklmn\r\npad   ");
        assert_eq!(term.get_line_text(0, None).as_deref(), Some("abcdefghi"));
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(
            copy_rows(&mut term, 0, 2).as_deref(),
            Some("abcdefghi jklmn\npad"),
            "drag over the wrapped line and the next"
        );

        let (first, last) = term.logical_line_span(0);
        let sel = term.text_selection_mut();
        sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
        sel.update_selection(last, 9, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("abcdefghi jklmn"),
            "triple-click's line selection"
        );

        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        assert_eq!(term.get_line_text(-3, None).as_deref(), Some("abcdefghi"));
        assert_eq!(
            copy_rows(&mut term, -3, -1).as_deref(),
            Some("abcdefghi jklmn\npad"),
            "the same rows copied from history"
        );
    }

    /// A line ending in blanks across a wrap is trimmed whole: `printf 'foo%12s'` in 10
    /// columns copies as `foo` (drag, triple-click, history), and an all-blank line copies
    /// as an empty line.
    #[test]
    fn copy_trims_a_line_that_ends_in_blanks_across_a_wrap() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(4, 10);
        term.process(format!("foo{:12}\r\nnext", "").as_bytes());
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(
            copy_rows(&mut term, 0, 2).as_deref(),
            Some("foo\nnext"),
            "drag"
        );

        let (first, last) = term.logical_line_span(0);
        let sel = term.text_selection_mut();
        sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
        sel.update_selection(last, 9, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("foo"),
            "triple-click"
        );

        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        assert_eq!(
            copy_rows(&mut term, -3, -1).as_deref(),
            Some("foo\nnext"),
            "from history"
        );

        let mut term = crate::terminal::Terminal::new(4, 10);
        term.process(format!("a\r\n{:15}\r\nb", "").as_bytes());
        assert_eq!(
            copy_rows(&mut term, 0, 3).as_deref(),
            Some("a\n\nb"),
            "a blank line"
        );
    }

    /// The unwritten cell a wide glyph's early wrap leaves adds no blank to the copy.
    #[test]
    fn copy_adds_no_blank_for_a_wide_glyphs_early_wrap() {
        let mut term = crate::terminal::Terminal::new(4, 10);
        term.process("x中中中中中y\r\n".as_bytes());
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(
            copy_rows(&mut term, 0, 1).as_deref(),
            Some("x中中中中中y"),
            "on screen"
        );
        term.process(b"1\r\n2\r\n3\r\n");
        assert_eq!(
            copy_rows(&mut term, -2, -1).as_deref(),
            Some("x中中中中中y"),
            "from history"
        );
    }

    /// With `ambiguous_width = "wide"` a scrollback copy of cells 4-6 (after two two-cell
    /// arrows) copies `foo`, as on the screen.
    #[test]
    fn a_scrollback_copy_measures_ambiguous_characters_as_the_grid_draws_them() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(4, 20);
        term.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        term.process("→→foo bar\r\n".as_bytes());
        let copy = |term: &mut crate::terminal::Terminal, row: i32| {
            let sel = term.text_selection_mut();
            sel.start_selection(row, 4, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(row, 6, SelectionSide::Right);
            sel.complete_selection();
            term.selection_to_string()
        };
        let smart = crate::selection::SmartSelection::with_builtin_rules();
        assert_eq!(copy(&mut term, 0).as_deref(), Some("foo"), "on screen");
        assert_eq!(
            term.smart_word_at(0, 5, &smart),
            Some((4, 7)),
            "the word under cell 5 on screen"
        );
        term.process(b"1\r\n2\r\n3\r\n4");
        assert_eq!(term.get_line_text(-1, None).as_deref(), Some("→→foo bar"));
        assert_eq!(copy(&mut term, -1).as_deref(), Some("foo"), "from history");
        let sel = term.text_selection_mut();
        sel.start_selection(-1, 4, SelectionSide::Left, SelectionType::Block);
        sel.update_selection(-1, 6, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("foo"),
            "a block copy from history"
        );
        term.scroll_display(1);
        assert_eq!(
            term.smart_word_at(0, 5, &smart),
            Some((4, 7)),
            "the word under cell 5 scrolled back"
        );
    }

    /// `smart_word_at` (a web host's double-click and link hit-test) finds the word in the
    /// cells the grid drew it in when two flag halves were drawn out of order: they stay
    /// two two-column cells, so `wx` is in cells 4-5 and the URL in 7-27.
    #[test]
    fn smart_word_at_finds_the_word_in_the_cells_the_grid_drew() {
        let mut term = crate::terminal::Terminal::new(4, 40);
        term.process(
            "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx https://example.com/a b".as_bytes(),
        );
        assert_eq!(
            term.cell_grapheme(0, 2).as_deref(),
            Some("\u{1f1e7}"),
            "precondition: the flag halves stay two cells"
        );
        assert_eq!(term.cell_grapheme(0, 7).as_deref(), Some("h"));
        let smart = crate::selection::SmartSelection::with_builtin_rules();
        for (col, want) in [
            (4, Some((4, 6))),
            (5, Some((4, 6))),
            (6, None),
            (7, Some((7, 28))),
            (27, Some((7, 28))),
            (29, Some((29, 30))),
            (30, None),
        ] {
            assert_eq!(term.smart_word_at(0, col, &smart), want, "column {col}");
        }
        // Scrolled into history and back into view: the ring tier draws the row from its
        // own cells, halves apart, and the words are read there.
        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        term.scroll_display(1);
        assert_eq!(
            term.display_cell_grapheme(0, 2).as_deref(),
            Some("\u{1f1e7}"),
            "precondition: the ring tier draws the halves as two cells"
        );
        for (col, want) in [
            (2, None),
            (4, Some((4, 6))),
            (5, Some((4, 6))),
            (7, Some((7, 28))),
            (29, Some((29, 30))),
        ] {
            assert_eq!(
                term.smart_word_at(0, col, &smart),
                want,
                "scrollback column {col}"
            );
        }
        assert_eq!(
            term.smart_word_text_at(0, 10, &smart),
            Some(("https://example.com/a".to_string(), 7, 28))
        );
    }

    /// The history tiers: the ring tier draws a row from its own cells; past the ring (a
    /// one-row ring) a row is drawn from its stored line.
    const HISTORY_TIERS: [(&str, Option<usize>); 2] =
        [("ring tier", None), ("past the ring", Some(1))];

    /// A terminal `rows` x `cols` (`ring` rows of ring, else the default) with `write` on
    /// its top row, then `before_scroll`, scrolled into history: the oldest history row,
    /// drawn from `tier`, and its selection-space row.
    fn row_in_history_tier(
        tier: &str,
        ring: Option<usize>,
        (rows, cols): (u16, u16),
        write: &str,
        before_scroll: impl FnOnce(&mut crate::terminal::Terminal),
    ) -> (crate::terminal::Terminal, i32) {
        use aterm_grid::test_counters::take_ring_fast_materialize;
        let mut term = match ring {
            None => crate::terminal::Terminal::new(rows, cols),
            Some(ring) => crate::terminal::Terminal::with_scrollback(
                rows,
                cols,
                ring,
                crate::scrollback::Scrollback::with_defaults(),
            ),
        };
        term.process(write.as_bytes());
        before_scroll(&mut term);
        term.process(format!("\x1b[{rows};1H{}", "\r\n".repeat(usize::from(rows))).as_bytes());
        let oldest = term.grid().scrollback_lines() - 1;
        let _ = take_ring_fast_materialize();
        let _ = term
            .grid()
            .materialize_scrollback_row_full(oldest, term.cols());
        assert_eq!(
            take_ring_fast_materialize(),
            usize::from(ring.is_none()),
            "{write:?}: precondition: the row is drawn from the {tier}"
        );
        let row = -i32::try_from(oldest + 1).expect("a short history");
        (term, row)
    }

    /// History cells written before a live switch to `ambiguous_width = "wide"` copy what
    /// they draw, on either tier: the row keeps its cells (an arrow is one), so cells 2-4
    /// are `foo`.
    #[test]
    fn a_history_copy_after_a_live_ambiguous_switch_copies_the_cells_drawn() {
        use crate::selection::{SelectionSide, SelectionType};
        for (tier, ring) in HISTORY_TIERS {
            let (mut term, row) =
                row_in_history_tier(tier, ring, (4, 20), "→→foo bar", |term| {
                    term.apply_config(&crate::config::TerminalConfig {
                        ambiguous_width_double: true,
                        ..Default::default()
                    });
                });
            assert_eq!(term.get_line_text(row, None).as_deref(), Some("→→foo bar"));
            term.scroll_to_top();
            assert_eq!(
                term.display_cell_grapheme(0, 2).as_deref(),
                Some("f"),
                "{tier}: precondition: the row keeps the cells it was drawn in"
            );
            term.scroll_to_bottom();
            assert_eq!(
                term.get_line_text(row, Some((2, 4))).as_deref(),
                Some("foo"),
                "{tier}: a block row"
            );
            for kind in [SelectionType::Simple, SelectionType::Block] {
                let sel = term.text_selection_mut();
                sel.start_selection(row, 2, SelectionSide::Left, kind);
                sel.update_selection(row, 4, SelectionSide::Right);
                sel.complete_selection();
                assert_eq!(
                    term.selection_to_string().as_deref(),
                    Some("foo"),
                    "{tier}: {kind:?}"
                );
            }
        }
    }

    /// A history row draws a flag whose halves a mark separates as the screen drew it (one
    /// cell, the live grid folding the second half into the first's cell), on either tier,
    /// so `ZEBRA` stays in cells 3-7 and a copy of them copies it.
    #[test]
    fn a_history_row_draws_a_flag_around_a_mark_as_the_screen_did() {
        let line = "\u{1f1fa}\u{301}\u{1f1f8} ZEBRA";
        let mut live = crate::terminal::Terminal::new(4, 20);
        live.process(line.as_bytes());
        let drawn = |term: &crate::terminal::Terminal, display: bool| -> Vec<String> {
            (0..10)
                .map(|col| {
                    if display {
                        term.display_cell_grapheme(0, col)
                    } else {
                        term.cell_grapheme(0, col)
                    }
                    .expect("in range")
                })
                .collect()
        };
        let on_screen = drawn(&live, false);
        assert_eq!(
            on_screen[3], "Z",
            "precondition: one flag cell, then a space"
        );
        for (tier, ring) in HISTORY_TIERS {
            let (mut term, row) = row_in_history_tier(tier, ring, (4, 20), line, |_| {});
            assert_eq!(
                term.get_line_text(row, Some((3, 7))).as_deref(),
                Some("ZEBRA"),
                "{tier}: a copy of the cells ZEBRA is drawn in"
            );
            term.scroll_to_top();
            assert_eq!(drawn(&term, true), on_screen, "{tier}");
        }
    }

    /// Every column of a history row's cells names the cell either tier draws there, for
    /// shapes whose text and cells disagree; a wide cell's right half names its cell, and a
    /// column no cell is read in draws nothing.
    #[test]
    fn a_history_rows_cells_name_the_cells_it_is_drawn_in() {
        let shapes = [
            "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yz",
            "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5Hwx yz",
            "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3Hwx yz",
            "\u{1f1e6}q\u{1f1e7}wx\x1b[1;3H\x1b[P",
            "\u{231a}\u{fe0e}wx \u{2764}\u{fe0f}\u{200d}Qa e\u{301}\u{302}f",
            "abcdefghi\u{2714}\u{fe0f}",
            "\u{4e00}\u{4e8c} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} z",
            "\u{1f1fa}\u{301}\u{1f1f8} wx",
            "\u{e9}\u{200d}\u{1f1fa}\u{1f1f8} wx",
            "plain ascii",
        ];
        for (tier, ring) in HISTORY_TIERS {
            for shape in shapes {
                let (mut term, row) = row_in_history_tier(tier, ring, (4, 10), shape, |_| {});
                let (text, cells) = term.written_line_cells(row).expect("a history row");
                term.scroll_to_top();
                for col in 0..10 {
                    let drawn = term.display_cell_grapheme(0, col).expect("in range");
                    let Some(byte) = cells.byte_at_column(col) else {
                        assert!(
                            drawn.trim().is_empty(),
                            "{tier} {shape:?}: column {col} draws {drawn:?}, no cell is read there"
                        );
                        continue;
                    };
                    if drawn.is_empty() {
                        assert_eq!(
                            col.checked_sub(1)
                                .and_then(|left| cells.byte_at_column(left)),
                            Some(byte),
                            "{tier} {shape:?}: the right half at column {col}"
                        );
                        continue;
                    }
                    assert!(
                        text[byte..].starts_with(&drawn),
                        "{tier} {shape:?}: column {col} names {:?}, the glass draws {drawn:?}",
                        &text[byte..]
                    );
                }
            }
        }
    }

    /// [`scrollback_line_range_text`] over a stored line laid out `cols` wide, as a row past
    /// the ring tier is drawn.
    fn stored_range_text(
        bytes: &[u8],
        ceiling: usize,
        cols: u16,
        range: (u16, u16),
        trailing: super::TrailingBlanks,
    ) -> String {
        let line = aterm_scrollback::Line::from_bytes(bytes);
        super::scrollback_line_range_text(
            bytes,
            ceiling,
            |text| crate::terminal::search_rows::stored_line_text_cells(&line, text, cols),
            range,
            trailing,
        )
    }

    #[test]
    fn scrollback_line_range_text_rows() {
        // (text, start col, end col inclusive, expected substring).
        let rows = [
            ("Hello", 0, 4, "Hello"),
            ("Hello World", 0, 4, "Hello"),
            ("Hello World", 6, 10, "World"),
            // Each CJK char is two columns: cols 0-1 = 你, cols 2-3 = 好.
            ("你好", 0, 1, "你"),
            ("你好", 0, 3, "你好"),
            // A glyph either edge cuts is copied whole.
            ("你好", 1, 2, "你好"),
            // A = col 0, 你 = cols 1-2, B = col 3.
            ("A你B", 1, 2, "你"),
            ("A你B", 2, 2, "你"),
            ("abc", 1, 1, "b"),
            // Start past the content.
            ("Hi", 10, 20, ""),
            ("", 0, 5, ""),
        ];
        for (text, start_col, end_col, want) in rows {
            assert_eq!(
                stored_range_text(
                    text.as_bytes(),
                    1024,
                    80,
                    (start_col, end_col),
                    super::TrailingBlanks::Written
                ),
                want,
                "{text:?} cols {start_col}..={end_col}"
            );
        }
    }

    // ── selection_to_string caps, at small values via selection_to_string_capped ──

    fn selection_grid() -> crate::terminal::Terminal {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(6, 10);
        term.process(b"R0\r\nR1\r\nR2\r\nR3\r\nR4\r\nR5");
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(5, 9, SelectionSide::Right);
        sel.complete_selection();
        term
    }

    #[test]
    fn selection_cap_rows_truncates_tail_preserving_start() {
        let term = selection_grid();
        let (text, truncated) = term.selection_to_string_capped(3, usize::MAX);
        let copied = text.expect("selection has text");
        assert!(truncated, "hitting the row cap sets the truncation flag");
        assert_eq!(
            copied.split('\n').count(),
            3,
            "row cap bounds the emitted rows to 3"
        );
        assert!(copied.starts_with("R0"), "oldest row (start) preserved");
        assert!(
            !copied.contains("R3"),
            "tail rows (R3..) dropped by the cap"
        );
    }

    #[test]
    fn push_row_capped_enforces_hard_byte_cap() {
        use super::push_row_capped;
        let mut r = String::new();
        assert!(
            !push_row_capped(&mut r, "abc", 10),
            "a row that fits is not clamped"
        );
        assert_eq!(r, "abc");
        assert!(
            push_row_capped(&mut r, "0123456789", 10),
            "an overflowing row is clamped"
        );
        assert_eq!(r.len(), 10, "hard cap: result never exceeds max_bytes");
        // Budget 2 < the 3-byte '你': nothing is pushed.
        let mut r2 = String::from("ab");
        assert!(push_row_capped(&mut r2, "你好", 4));
        assert_eq!(
            r2, "ab",
            "no partial multi-byte char pushed when budget < char width"
        );
        assert!(r2.is_char_boundary(r2.len()));
    }

    #[test]
    fn selection_byte_cap_is_hard_within_a_single_row() {
        // A cap below the first row's width clamps mid-row.
        let term = selection_grid();
        let (text, truncated) = term.selection_to_string_capped(usize::MAX, 1);
        let copied = text.expect("selection has text");
        assert_eq!(
            copied, "R",
            "mid-row hard cap: exactly max_bytes, not the whole row"
        );
        assert!(truncated, "clamping a single row sets the truncation flag");
    }

    #[test]
    fn scrollback_line_range_text_bounds_scan_to_ceiling() {
        use super::TrailingBlanks;
        let bytes = vec![b'A'; 2000];

        let read = |start, end, ceiling| {
            stored_range_text(&bytes, ceiling, 2048, (start, end), TrailingBlanks::Trim)
        };
        assert_eq!(read(0, 9, 100).len(), 10);

        assert!(
            read(500, 509, 100).is_empty(),
            "columns past the scan ceiling must not be materialized"
        );

        // Reachable under a higher ceiling: the emptiness above is the ceiling's.
        assert_eq!(read(500, 509, 4096).len(), 10);
    }

    #[test]
    fn scrollback_line_range_text_lossy_prefix_is_bounded() {
        use super::TrailingBlanks;
        // Invalid UTF-8 forces the lossy path, which still reads only a bounded prefix.
        let mut bytes = vec![0xFFu8; 4096]; // invalid UTF-8, 4096 bytes
        bytes.extend_from_slice(b"tail");
        let out = stored_range_text(&bytes, 64, 80, (0, 3), TrailingBlanks::Trim);
        assert!(out.chars().count() <= 4, "bounded to the requested columns");
        assert!(
            !out.contains("tail"),
            "content past the ceiling is never scanned"
        );
    }

    #[test]
    fn get_line_text_extracts_from_large_injected_scrollback_line() {
        use crate::grid::Grid;
        use crate::scrollback::{Line, Scrollback};
        // One 2 MiB line, twice the scan ceiling, as a checkpoint restore can leave.
        let big = vec![b'A'; 2 * 1024 * 1024];
        let mut scrollback = Scrollback::with_defaults();
        scrollback.push_line(Line::from_bytes(&big));
        let grid = Grid::with_tiered_scrollback(4, 80, 1000, scrollback);
        let term = crate::terminal::Terminal::with_grid(grid);

        let text = term
            .get_line_text(-1, Some((0, 79)))
            .expect("scrollback line has text");
        assert_eq!(text.len(), 80, "extraction bounded to the 80-column range");
        assert!(
            text.bytes().all(|b| b == b'A'),
            "content preserved within the range"
        );
    }

    /// `get_line_text`'s positive rows are terminal-relative: row 0 is the live top at any
    /// scroll position, as the renderer's `sel_row = viewport_row - display_offset` assumes.
    #[test]
    fn get_line_text_positive_rows_are_scroll_invariant() {
        let mut term = crate::terminal::Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        let live: Vec<Option<String>> = (0..5).map(|r| term.get_line_text(r, None)).collect();
        assert_eq!(live[0].as_deref(), Some("line26"), "precondition: live top");

        term.scroll_display(3);
        for (r, want) in live.iter().enumerate() {
            assert_eq!(
                &term.get_line_text(r as i32, None),
                want,
                "live row {r} must read the same while scrolled back"
            );
        }
    }

    /// The copied text does not change when the viewport scrolls between drag and copy.
    #[test]
    fn selection_copy_is_scroll_invariant() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        let sel = term.text_selection_mut();
        sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 19, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(term.selection_to_string().as_deref(), Some("line28"));

        term.scroll_display(2);
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("line28"),
            "a viewport scroll must not change what an existing selection copies"
        );
    }

    /// A docker command soft-wrapped across two 80-column rows, then a hard-newline line.
    fn wrapped_docker_fixture() -> (crate::terminal::Terminal, String) {
        const HEAD: &str =
            "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
        const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";
        let cmd = format!("{HEAD}{TAIL}");
        assert_eq!(
            HEAD.len(),
            80,
            "the head must fill the 80-column row exactly"
        );
        let mut term = crate::terminal::Terminal::new(6, 80);
        term.process(cmd.as_bytes());
        term.process(b"\r\nAFTER");
        (term, cmd)
    }

    /// The logical line containing `row` as a double-click reads it: `(text, first, last)`.
    fn logical_line(term: &crate::terminal::Terminal, row: i32) -> Option<(String, i32, i32)> {
        let (first, last) = term.logical_line_span(row);
        let (text, _) = term.line_range_cells(first, last)?;
        Some((text, first, last))
    }

    /// Every column `g` of the joined rows `first..=last` names the cell
    /// `(first + g / cols, g % cols)` the grid drew there; a wide glyph's right half names
    /// its left half, and only the blanks past the last row's text are in no cell.
    fn assert_join_names_the_drawn_cells(term: &crate::terminal::Terminal, first: i32, last: i32) {
        let cols = usize::from(term.cols());
        let (text, cells) = term.line_range_cells(first, last).expect("rows to join");
        let rows = usize::try_from(last - first + 1).expect("an ordered span");
        for g in 0..rows * cols {
            let row = usize::try_from(first).expect("a live row") + g / cols;
            let drawn = term.cell_grapheme(row, g % cols).expect("in range");
            let byte = cells.byte_at_column(g);
            if drawn.is_empty() {
                assert_eq!(
                    byte,
                    cells.byte_at_column(g - 1),
                    "column {g}: a right half names its left half"
                );
                continue;
            }
            let Some(byte) = byte else {
                assert!(
                    g >= cells.end_col() && drawn == " ",
                    "column {g}: no cell in {text:?}"
                );
                continue;
            };
            assert!(
                text[byte..].starts_with(&drawn),
                "column {g} of {text:?} must name cell ({row}, {}) holding {drawn:?}",
                g % cols
            );
        }
    }

    /// The join names the cells the grid drew, row `k` from column `k * cols`.
    #[test]
    fn line_range_cells_name_the_cells_across_the_wrap() {
        let (term, cmd) = wrapped_docker_fixture();
        let (text, first, last) = logical_line(&term, 0).expect("row 0 has text");
        assert_eq!((first, last), (0, 1));
        assert_eq!(text, cmd, "the joined line must be the whole command");
        assert_join_names_the_drawn_cells(&term, first, last);
        assert_eq!(
            logical_line(&term, 2),
            Some(("AFTER".to_string(), 2, 2)),
            "an unwrapped row is its own logical line, text unchanged"
        );
    }

    /// The join names the cells the grid drew where a row's text measures otherwise: two
    /// flag halves drawn out of order stay two cells, and a VS16 base in the last column
    /// stays one.
    #[test]
    fn line_range_cells_name_the_cells_where_a_row_measures_otherwise() {
        let mut term = crate::terminal::Terminal::new(4, 10);
        term.process(
            "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yzpqr abcdef\u{2714}\u{fe0f}gh ij"
                .as_bytes(),
        );
        assert_eq!(term.cell_grapheme(0, 2).as_deref(), Some("\u{1f1e7}"));
        assert_eq!(
            term.cell_grapheme(1, 9).as_deref(),
            Some("\u{2714}\u{fe0f}"),
            "precondition: the base and its VS16 are one cell in the last column"
        );
        assert_eq!(term.logical_line_span(0), (0, 2), "precondition: one run");
        assert_join_names_the_drawn_cells(&term, 0, 2);
        // Each window of the run, as a double-click widens over it.
        assert_join_names_the_drawn_cells(&term, 1, 2);
        assert_join_names_the_drawn_cells(&term, 0, 1);

        // The same from scrollback: the stored VS16 base stays one cell in the last column.
        let mut term = crate::terminal::Terminal::new(3, 10);
        term.process("abcdefghi\u{2714}\u{fe0f}jk lm".as_bytes());
        let live = term.line_range_cells(0, 1).expect("a live run");
        term.process(b"\r\n1\r\n2\r\n3");
        assert_eq!(term.logical_line_span(-1), (-2, -1));
        assert_eq!(term.line_range_cells(-2, -1), Some(live));
    }

    /// A history row whose stored line is wider than the grid (a checkpoint restore can
    /// leave one) gives only the text its cells draw, so a double-click's window stays as
    /// long as the rows it spans.
    #[test]
    fn line_range_cells_reads_only_the_text_a_rows_cells_draw() {
        use crate::grid::Grid;
        use crate::scrollback::{Line, Scrollback};
        let big = vec![b'A'; 2 * 1024 * 1024];
        let mut scrollback = Scrollback::with_defaults();
        scrollback.push_line(Line::from_bytes(&big));
        let grid = Grid::with_tiered_scrollback(4, 80, 1000, scrollback);
        let mut term = crate::terminal::Terminal::with_grid(grid);
        let (text, cells) = term.line_range_cells(-1, -1).expect("a history row");
        assert_eq!(text, "A".repeat(80), "the 80 cells the row draws");
        assert_eq!((cells.end_col(), cells.end_byte()), (80, 80));
        term.scroll_display(1);
        let smart = crate::selection::SmartSelection::with_builtin_rules();
        assert_eq!(
            term.smart_word_text_at(0, 5, &smart),
            Some(("A".repeat(80), 0, 80))
        );
    }

    /// A logical line straddling the scrollback/live boundary joins across it.
    #[test]
    fn logical_line_span_joins_across_the_scrollback_boundary() {
        let mut term = crate::terminal::Terminal::new(2, 10);
        // Wraps to two rows; more lines push the head into history.
        term.process(b"ABCDEFGHIJKL");
        term.process(b"\r\nsecond\r\nthird");
        assert_eq!(
            term.get_line_text(-1, None).as_deref(),
            Some("KL"),
            "precondition: the wrapped TAIL is the newest history row"
        );
        assert_eq!(
            term.logical_line_span(-1),
            (-2, -1),
            "the span must reach back into scrollback for the wrapped head"
        );
        assert_eq!(
            logical_line(&term, -1).map(|(t, ..)| t).as_deref(),
            Some("ABCDEFGHIJKL"),
            "and join the two history rows into the whole logical line"
        );
    }
}
