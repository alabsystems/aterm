// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text selection to string conversion for `Terminal`: `selection_to_string()`,
//! `get_line_text()` and the logical-line helpers the click gestures use.

use std::borrow::Cow;

use aterm_types::text_shaping::AmbiguousWidth;

use super::Terminal;
use super::content::{TrailingBlanks, visible_row_bounds_to_string};

/// Cap on the rows [`Terminal::selection_to_string`] materializes in one call.
///
/// Scrollback retention can be unlimited, so without a cap a control-socket
/// select-all plus copy would do O(scrollback) work under the terminal mutex.
/// 1M rows is about ten times the default 100k retention and the same order as
/// search's `MAX_SEARCH_MATCHES`, so realistic selections are never clipped.
const MAX_SELECTION_ROWS: usize = 1_000_000;

/// Cap on the bytes [`Terminal::selection_to_string`] produces: the binding
/// memory bound, since one stored line can be `MAX_GRID_COLS` (4096) wide
/// clusters of multi-byte text. 64 MiB exceeds a full default 100k-line
/// scrollback (~20 MiB).
const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;

/// Cap, in each direction, on how many physical rows
/// [`Terminal::logical_line_span`] walks to join one soft-wrapped logical line.
/// Each history row's wrap bit can cost a tier fetch under the terminal mutex,
/// and scrollback can be unlimited. 4096 rows (~320 KiB at 80 columns) never
/// clips a line a human selects.
const MAX_LOGICAL_LINE_ROWS: usize = 4096;

/// Per-line byte ceiling for scrollback text extraction in
/// [`Terminal::get_line_text`].
///
/// A scrollback [`Line`](aterm_scrollback::Line) has no length bound (a crafted
/// checkpoint restored via `restore_grid` -> `deserialize_lines` can inject a
/// multi-MiB line), so extraction bounds both the copy and the grapheme walk.
/// A real line is at most `MAX_GRID_COLS` (4096) cells of 1 base +
/// `Extra::MAX_COMBINING` (16) marks (~272 KiB), so 1 MiB never clips one.
pub(crate) const MAX_SCROLLBACK_LINE_SCAN_BYTES: usize = 1024 * 1024;

/// A stored line's full text (all columns, no trim) bounded to
/// `max_scan_bytes`: a [`Line::to_string`](aterm_scrollback::Line) equivalent
/// that an oversized line cannot blow up. Used by the search-index build.
/// Borrows valid UTF-8 under the ceiling; lossy-converts only a bounded prefix
/// otherwise.
pub(crate) fn line_text_bounded(bytes: &[u8], max_scan_bytes: usize) -> Cow<'_, str> {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    String::from_utf8_lossy(scan)
}

/// Convert a column range to byte offsets in one grapheme walk.
///
/// - `start_col`: first column (inclusive)
/// - `end_col`: last column (inclusive); the returned end byte is one past this grapheme
///
/// Returns `(start_byte, end_byte)` for `&s[start_byte..end_byte]`, or
/// `(s.len(), s.len())` if `start_col` is past all content.
///
/// The scrollback twin of [`visible_row_bounds_to_string`]'s
/// [`glyph_cell_span`](aterm_types::selection::glyph_cell_span) edges: the walk
/// carries each cluster's width, so a glyph straddling `start_col` or `end_col`
/// is admitted whole, and a stored line copies the same cells the highlight
/// paints.
///
/// [`visible_row_bounds_to_string`]: super::content::visible_row_bounds_to_string
fn column_range_to_byte_offsets(
    s: &str,
    start_col: usize,
    end_col: usize,
    ambiguous: AmbiguousWidth,
) -> (usize, usize) {
    // Grid columns, not each cluster's display width: outside emoji the grid
    // advances per character (a Devanagari conjunct spans 3 cells) and folds
    // across cluster boundaries (Sinhala `ක්‍ය` is two clusters in one cell),
    // so a cluster is copied when any column it is drawn in is selected. An
    // East Asian Ambiguous character takes the cells the `ambiguous_width`
    // policy draws it in.
    let mut range: Option<(usize, usize)> = None;
    for g in aterm_grapheme::grid_graphemes_with(s, ambiguous) {
        if g.start_col > end_col {
            break;
        }
        if g.end_col > start_col {
            let end = g.byte_offset + g.text.len();
            range = Some(range.map_or((g.byte_offset, end), |(start, _)| (start, end)));
        }
    }
    range.unwrap_or((s.len(), s.len()))
}

/// Extract a column range from a scrollback line's raw bytes without
/// materializing the whole line. Both the copy and the grapheme walk are
/// bounded by `max_scan_bytes` ([`MAX_SCROLLBACK_LINE_SCAN_BYTES`] in
/// production); columns past the scanned content yield an empty string. Split
/// from [`Terminal::get_line_text`] so the ceiling can be tested small.
///
/// A stored line holds exactly the cells its row had written, so
/// [`TrailingBlanks::Written`] keeps the range as stored.
fn scrollback_line_range_text(
    bytes: &[u8],
    start_col: u16,
    end_col: u16,
    max_scan_bytes: usize,
    trailing: TrailingBlanks,
    ambiguous: AmbiguousWidth,
) -> String {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    let line_lossy = String::from_utf8_lossy(scan);
    let line: &str = &line_lossy;
    let (start_byte, end_byte) = column_range_to_byte_offsets(
        line,
        usize::from(start_col),
        usize::from(end_col),
        ambiguous,
    );
    if start_byte >= line.len() {
        return String::new();
    }
    let range = &line[start_byte..end_byte];
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

impl Terminal {
    /// Get the selected text as a string; `None` if there is no selection or it
    /// is empty. Block selections separate rows with newlines.
    ///
    /// Output is bounded by [`MAX_SELECTION_ROWS`] / [`MAX_SELECTION_BYTES`];
    /// clipped content is dropped silently here. Use
    /// [`Self::selection_to_string_bounded`] to learn whether truncation occurred.
    #[must_use]
    pub fn selection_to_string(&self) -> Option<String> {
        self.selection_to_string_bounded().0
    }

    /// Like [`Self::selection_to_string`], but also returns whether content is
    /// missing from the text, so callers can report truncation (as search
    /// reports `incomplete`). `true` means selected content is absent from either
    /// end: the copy caps drop it from the tail, and a scrollback eviction that
    /// clamped the selection's head drops it from the front.
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

        // Side-adjusted bounds so the copied text matches the highlight (a
        // Right-sided start or Left-sided end excludes its character).
        // `truncated` also covers eviction, decided before the walk: a clamped
        // head at the floor row, col 0, side Left can collapse the span, and
        // that must still report incomplete rather than an empty `OK 0`.
        let evicted = self.text_selection.truncated();
        let Some((adj_start_row, adj_start_col, adj_end_row, adj_end_col)) =
            self.text_selection.side_adjusted_bounds()
        else {
            return (None, evicted);
        };

        let mut result = String::new();
        let cols = self.grid.cols();
        if cols == 0 {
            // A zero-column grid addresses no content at all; that is a degenerate
            // geometry, not a partial report about a span that exists.
            return (None, false);
        }

        // Two bounds keep any selection from spinning or allocating unboundedly
        // under the terminal mutex:
        // 1. Clamp the row span to [-scrollback_lines, rows-1], so unclamped i32
        //    anchors from the control socket do not iterate rows with no content.
        // 2. MAX_SELECTION_ROWS / MAX_SELECTION_BYTES in the loops below, since
        //    real content itself can be unbounded (unlimited scrollback).
        let visible_rows = i32::from(self.grid.rows());
        let history = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
        let first_row = adj_start_row.max(-history);
        let last_row = adj_end_row.min(visible_rows - 1);

        // One geometry value, computed once and threaded to whichever shape walk
        // runs. Deriving it inside the walks instead would let the two copies drift.
        let geom = SelectionGeometry {
            first_row,
            last_row,
            adj_start_row,
            adj_end_row,
            adj_start_col,
            adj_end_col,
            cols,
        };

        // Iteration runs oldest to newest, so a cap truncates the tail and the
        // start row/col is always kept. Each shape walk returns its counters so
        // the cap report below is the same for either shape.
        let (rows_emitted, truncated) = match self.text_selection.selection_type() {
            SelectionType::Block => {
                self.push_block_selection(&mut result, geom, max_rows, max_bytes)
            }
            // Simple, Semantic, Lines, and future variants all use linear selection
            _ => self.push_linear_selection(&mut result, geom, max_rows, max_bytes),
        };

        if truncated {
            // Log the truncation for callers that only take the text (mouse, wasm);
            // `truncated` is also returned so cmd_selection/cmd_copy can append
            // ` incomplete`.
            aterm_log::warn!(
                "selection_to_string truncated at cap (rows_emitted={rows_emitted}, bytes={})",
                result.len()
            );
        }

        // The two halves of the report are independent — a capped walk over an
        // already-clamped selection is missing text at both ends — so they or.
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

        // Rectangular selection: extract adjusted columns from each row
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
    /// Trailing blanks are trimmed where a logical line ends (a hard line break,
    /// or the selection's last row), from the whole line: a row the next row
    /// continues (a soft wrap) keeps every cell the program wrote, so a space it
    /// printed in the last column joins the two words it separates, and those
    /// kept blanks go when the line ends in blanks after all. The cell a wide
    /// glyph leaves when it wraps early was never written, so it is not in the
    /// row's written text either: no guess about holes is needed.
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
        // Whether a row's trailing blanks are content depends on the NEXT row's
        // wrap bit, so the next row is read one iteration early and carried
        // into its own: each history row is still fetched from the tier once (a
        // fetch builds a full `Line` on the ring path, or clones one out of a
        // decompressed block).
        let mut lookahead: Option<SelectionRow<'_>> = None;
        // Where the logical line now being emitted starts in `result`: its end
        // trims back to here, never into the line before it.
        let mut line_start = result.len();

        for row in first_row..=last_row {
            if rows_emitted >= max_rows || result.len() >= max_bytes {
                truncated = true;
                break;
            }
            rows_emitted += 1;
            let current = lookahead.take().unwrap_or_else(|| self.selection_row(row));
            // A newline only before a row that does not continue the one above
            // (`continuation_bit`, shared with the logical-line click gestures).
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
            // `row < last_row` also means the range runs to the row's last
            // column: only the end row stops short of it.
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

            // The history arm is `get_line_text`'s negative-row arm on the line
            // already fetched, with the same `MAX_SCROLLBACK_LINE_SCAN_BYTES`
            // ceiling.
            let line = if row < 0 {
                current.history.as_ref().map(|stored| {
                    scrollback_line_range_text(
                        stored.as_bytes(),
                        start_col,
                        end_col,
                        MAX_SCROLLBACK_LINE_SCAN_BYTES,
                        trailing,
                        self.ambiguous_width(),
                    )
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
            // The logical line ends here: trim it whole, so blanks a wrapped row
            // kept are dropped when nothing but blanks follows them (a padded
            // column wider than the window copies as its text, as before).
            if trailing == TrailingBlanks::Trim {
                let keep = line_start + result[line_start..].trim_end().len();
                result.truncate(keep);
            }
        }

        (rows_emitted, truncated)
    }

    /// Read selection-space `row` for [`Self::push_linear_selection`]: its
    /// stored line when it is history (fetched once, here) and whether it
    /// continues the row above.
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

    /// Whether selection-space `row` continues the previous row's content (a
    /// soft wrap, not a hard line break).
    ///
    /// The one place selection reads `Row::is_wrapped()` / `Line::is_wrapped()`:
    /// [`Self::push_linear_selection`] (newlines in the copy) and
    /// [`Self::logical_line_span`] (click reach) must agree, or the highlight and
    /// the clipboard describe different text.
    ///
    /// `history_line` is the caller's already-fetched line for a negative row;
    /// with `None` this fetches it. Reads the live frame: selection rows are
    /// terminal-relative (see [`Self::get_line_text`]).
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

    /// The inclusive physical row span `(first, last)` of the logical line
    /// containing selection-space `row`: the maximal run joined by soft wraps.
    ///
    /// This is what word/line click gestures select, matching the copy walk
    /// ([`Self::push_linear_selection`]), which emits no newline before a
    /// continuation row. Bounding a triple-click to the physical row would drop
    /// the continuation from the clipboard.
    ///
    /// A hard newline ends the run in both directions; an unwrapped row answers
    /// `(row, row)`. The walk stops at the live screen's bottom and at the
    /// scrollback floor (an evicted head starts at the oldest retained row, as
    /// `adjust_for_scroll` clamps), and an out-of-range row answers `(row, row)`.
    /// Bounded by [`MAX_LOGICAL_LINE_ROWS`] in each direction.
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

    /// The column-aligned text of one window of a logical line: rows
    /// `first..=last`, already clamped to the line's span. Same alignment as
    /// `Self::logical_line_text` (every row but the last contributes exactly
    /// `cols` columns), so `g / cols`, `g % cols` map back to a cell relative to
    /// `first`.
    ///
    /// Word rules scan only this window, widening it when a match runs to an
    /// edge, so a double-click on a very long logical line (a minified bundle)
    /// does not scan the whole join under the terminal mutex.
    #[must_use]
    pub fn line_range_text(&self, first: i32, last: i32) -> Option<String> {
        let own = self.get_line_text(first, None)?;
        if first == last {
            return Some(own);
        }
        let cols = usize::from(self.grid.cols());
        let ambiguous = self.ambiguous_width();
        // Measure each part, not the accumulating buffer: `byte_to_column` is
        // linear in its input, so measuring the join per row would be quadratic.
        let rows = usize::try_from(last.saturating_sub(first))
            .unwrap_or(0)
            .saturating_add(1);
        let mut text = String::with_capacity(rows.saturating_mul(cols.saturating_add(1)));
        let mut part = own;
        for r in first..=last {
            if r > first {
                part = self.get_line_text(r, None).unwrap_or_default();
            }
            let width = aterm_grapheme::byte_to_column_with(&part, part.len(), ambiguous);
            text.push_str(&part);
            if r < last {
                for _ in width..cols {
                    text.push(' ');
                }
            }
        }
        Some(text)
    }

    /// The column-aligned text of the logical line containing selection-space
    /// `row`, with its span: `(text, first_row, last_row)`.
    ///
    /// Every row but the last contributes exactly `cols` columns (padded with
    /// spaces where extraction trimmed trailing blanks), so a column maps back
    /// to a cell by:
    ///
    /// ```text
    /// row = first_row + g / cols        col = g % cols
    /// ```
    ///
    /// This lets double-click run the smart-selection rules over the whole
    /// logical line (a word straddling the wrap is one word; at a ragged wrap
    /// the padding reads as a break) and map the boundaries back to cells. The
    /// padding is a coordinate fixup; the copy path re-extracts its cells.
    ///
    /// `None` only when `row` addresses no line (as [`Self::get_line_text`]).
    #[must_use]
    #[cfg(test)]
    pub fn logical_line_text(&self, row: i32) -> Option<(String, i32, i32)> {
        let own = self.get_line_text(row, None)?;
        let (first, last) = self.logical_line_span(row);
        if first == last {
            return Some((own, first, last));
        }
        let cols = usize::from(self.grid.cols());
        let ambiguous = self.ambiguous_width();
        let mut text = String::new();
        for r in first..=last {
            let part = if r == row {
                own.clone()
            } else {
                self.get_line_text(r, None).unwrap_or_default()
            };
            let width = aterm_grapheme::byte_to_column_with(&part, part.len(), ambiguous);
            text.push_str(&part);
            if r < last {
                for _ in width..cols {
                    text.push(' ');
                }
            }
        }
        Some((text, first, last))
    }

    /// Get text from a line (visible or scrollback), trailing blanks trimmed.
    ///
    /// `row` is terminal-relative and scroll-invariant: `0..rows` are the live
    /// screen rows (the renderer paints a selection at `viewport_row -
    /// display_offset`), negative rows are scrollback (`-1` = newest history
    /// line). `col_range` is inclusive; `None` extracts the whole line.
    pub fn get_line_text(&self, row: i32, col_range: Option<(u16, u16)>) -> Option<String> {
        self.line_text(row, col_range, TrailingBlanks::Trim)
    }

    /// The text a row holds as written, trailing blanks included: every cell a
    /// live row's program wrote (up to `Row::len`), and a history row's stored
    /// line whole (bounded by [`MAX_SCROLLBACK_LINE_SCAN_BYTES`]), which is what
    /// that row held when it scrolled off. `None` when `row` addresses no line.
    ///
    /// One extractor for both, so a row reads the same on the screen and in
    /// history: what search indexes, and what a `$`-anchored or space-ended
    /// query sees, does not change when output scrolls the row off the screen.
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
            // Visible row: row is in [0, visible_rows) where visible_rows <= u16::MAX
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
            // Scrollback row: history_line_rev covers the ring buffer and tiered
            // scrollback; rev_idx 0 is the most recently scrolled-off line.
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            if let Some(scrollback_line) = self.grid.history_line_rev(rev_idx) {
                let cols = self.grid.cols();
                if cols == 0 && col_range.is_none() {
                    return Some(String::new());
                }
                let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));

                // Extract without materializing the whole line: a stored line's
                // length is unbounded (see MAX_SCROLLBACK_LINE_SCAN_BYTES).
                return Some(scrollback_line_range_text(
                    scrollback_line.as_bytes(),
                    start_col,
                    end_col,
                    MAX_SCROLLBACK_LINE_SCAN_BYTES,
                    trailing,
                    self.ambiguous_width(),
                ));
            }
            None
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AmbiguousWidth, column_range_to_byte_offsets};

    /// The copy is correct and every selected history row is fetched from the
    /// tier exactly once: `take_row_to_line_ops` counts `Line` constructions
    /// (`row_to_line_with_stored_extras`), and an N-row selection must cost N.
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
        // Ring reads/writes the fixture performed are not what is being counted.
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

        // Identity first: history holds fill lines 0..=(FILL - ROWS), so row -1
        // is fill line FILL - ROWS and row -SELECTED is SELECTED-1 lines above it.
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

    /// Select rows `first..=last` whole, as a drag over them does, and copy.
    fn copy_rows(term: &mut crate::terminal::Terminal, first: i32, last: i32) -> Option<String> {
        use crate::selection::{SelectionSide, SelectionType};
        let cols = term.cols();
        let sel = term.text_selection_mut();
        sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(last, cols - 1, SelectionSide::Right);
        sel.complete_selection();
        term.selection_to_string()
    }

    /// A space the program printed in a wrapped row's last column separates two
    /// words, so the copy keeps it at the join: on screen, from history, and for
    /// a triple-click's line selection. A row that ends its line still loses
    /// its trailing blanks.
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

    /// The same at 80 columns, where dropping the space would fuse a command's
    /// two arguments, and in prose wrapped mid-sentence.
    #[test]
    fn copy_keeps_wrap_boundary_spaces_in_prose_and_commands() {
        let cmd = "cp ./build/output/release/artifacts/aterm-universal-apple-darwin.tar.zstxxxxxxx \
                   /Volumes/backup/";
        assert_eq!(
            cmd.find(" /"),
            Some(79),
            "the space lands in the last column"
        );
        let mut term = crate::terminal::Terminal::new(4, 80);
        term.process(cmd.as_bytes());
        assert_eq!(copy_rows(&mut term, 0, 1).as_deref(), Some(cmd));

        let mut term = crate::terminal::Terminal::new(4, 10);
        term.process(b"aaaa bbbb cccc dddd");
        assert_eq!(
            copy_rows(&mut term, 0, 1).as_deref(),
            Some("aaaa bbbb cccc dddd")
        );
    }

    /// A line that ends in blanks across a wrap is trimmed whole, as it was
    /// before wrapped rows kept their blanks: `printf 'foo%12s'` in 10 columns
    /// (a padded column wider than the window) copies as `foo`, by drag, by
    /// triple-click and from history, and a line of nothing but blanks copies
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

    /// The cell a wide glyph leaves when it wraps early was never written, so
    /// the copy keeps no blank for it: the program wrote `x中中中中中y`.
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

    /// With `ambiguous_width = "wide"` a scrollback copy of the cells a line's
    /// `foo` is drawn in (4-6, after two two-cell arrows) copies `foo`, as the
    /// same cells copy on the screen.
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

    #[test]
    fn column_range_to_byte_offsets_rows() {
        // (text, start col, end col inclusive, expected substring). An empty
        // expectation means both offsets sit at the end of the text.
        let rows = [
            ("Hello", 0, 4, "Hello"),
            ("Hello World", 0, 4, "Hello"),
            ("Hello World", 6, 10, "World"),
            // Each CJK char is two columns: cols 0-1 = 你, cols 2-3 = 好.
            ("你好", 0, 1, "你"),
            ("你好", 0, 3, "你好"),
            // A = col 0, 你 = cols 1-2, B = col 3.
            ("A你B", 1, 2, "你"),
            ("abc", 1, 1, "b"),
            // Start past the content.
            ("Hi", 10, 20, ""),
            ("", 0, 5, ""),
        ];
        for (text, start_col, end_col, want) in rows {
            let (s, e) =
                column_range_to_byte_offsets(text, start_col, end_col, AmbiguousWidth::Single);
            assert_eq!(&text[s..e], want, "{text:?} cols {start_col}..={end_col}");
            if want.is_empty() {
                assert_eq!((s, e), (text.len(), text.len()), "{text:?}");
            }
        }
    }

    // ── selection_to_string caps ─────────────────────────────────────────────
    //
    // The production caps are too large for a fast test, so these call
    // selection_to_string_capped with small caps: the row cap truncates the tail
    // and keeps the start row/col, the byte cap truncates, and caps above the
    // content match the uncapped public method.

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
    fn selection_cap_bytes_truncates_tail() {
        let term = selection_grid();
        let (text, truncated) = term.selection_to_string_capped(usize::MAX, 5);
        let copied = text.expect("selection has text");
        assert!(truncated, "hitting the byte cap sets the truncation flag");
        assert!(copied.starts_with("R0"), "start preserved");
        assert!(
            !copied.contains("R5"),
            "newest rows dropped once the byte cap is hit"
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
        // Never split a multi-byte char: budget 2 < 3-byte '你' -> push nothing.
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
        // max_bytes smaller than the first row's width: the cap must clamp mid-row
        // (not overshoot) and flag truncation. Row 0 is "R0" (2 bytes); cap = 1.
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
    fn selection_no_truncation_under_caps_is_byte_identical() {
        let term = selection_grid();
        // Caps far above the content must not alter output vs the uncapped method.
        let (capped, truncated) = term.selection_to_string_capped(1_000, 1_000_000);
        let full = term.selection_to_string();
        assert!(!truncated, "generous caps do not flag truncation");
        assert_eq!(capped, full, "generous caps are output-preserving");
        assert!(
            full.as_deref().unwrap_or_default().contains("R5"),
            "every selected row is present when under the caps"
        );
    }

    // ── scrollback line extraction bounds the scan to the ceiling ───────────
    //
    // scrollback_line_range_text reads at most `max_scan_bytes` of a line (a
    // crafted checkpoint can inject a multi-MiB one). Tested at a small ceiling.
    #[test]
    fn line_text_bounded_borrows_valid_utf8_and_owns_lossy_text() {
        use std::borrow::Cow;

        use super::line_text_bounded;

        let valid = line_text_bounded(b"alpha beta", 1024);
        assert!(
            matches!(valid, Cow::Borrowed("alpha beta")),
            "valid UTF-8 should stay borrowed"
        );

        let truncated = line_text_bounded(b"alpha beta", 5);
        assert!(
            matches!(truncated, Cow::Borrowed("alpha")),
            "a valid bounded prefix should also stay borrowed"
        );

        let lossy = line_text_bounded(b"alpha\xff", 1024);
        match lossy {
            Cow::Owned(text) => assert_eq!(text, "alpha\u{fffd}"),
            Cow::Borrowed(_) => panic!("invalid UTF-8 must use the owned lossy path"),
        }
    }

    #[test]
    fn scrollback_line_range_text_bounds_scan_to_ceiling() {
        use super::{AmbiguousWidth, TrailingBlanks, scrollback_line_range_text};
        // 2000 single-byte, single-width columns; ceiling of 100 bytes.
        let bytes = vec![b'A'; 2000];

        // In-range within the ceiling: cols 0..9 → 10 'A's.
        let read = |start, end, ceiling| {
            scrollback_line_range_text(
                &bytes,
                start,
                end,
                ceiling,
                TrailingBlanks::Trim,
                AmbiguousWidth::Single,
            )
        };
        assert_eq!(read(0, 9, 100).len(), 10);

        // Columns past the 100-byte ceiling are not materialized, so empty.
        assert!(
            read(500, 509, 100).is_empty(),
            "columns past the scan ceiling must not be materialized"
        );

        // With a ceiling above the requested column, the same request is reachable
        // — proving the emptiness above is the ceiling, not a logic bug.
        assert_eq!(read(500, 509, 4096).len(), 10);
    }

    #[test]
    fn scrollback_line_range_text_lossy_prefix_is_bounded() {
        use super::{AmbiguousWidth, TrailingBlanks, scrollback_line_range_text};
        // Invalid UTF-8 forces the lossy path; a >ceiling line must still only touch
        // a bounded prefix (no panic, bounded output).
        let mut bytes = vec![0xFFu8; 4096]; // invalid UTF-8, 4096 bytes
        bytes.extend_from_slice(b"tail");
        let out = scrollback_line_range_text(
            &bytes,
            0,
            3,
            64,
            TrailingBlanks::Trim,
            AmbiguousWidth::Single,
        );
        // 64-byte prefix of 0xFF → up to 64 U+FFFD (3 bytes each); cols 0..3 → 4.
        assert!(out.chars().count() <= 4, "bounded to the requested columns");
        assert!(
            !out.contains("tail"),
            "content past the ceiling is never scanned"
        );
    }

    // ── the real checkpoint-restore path extracts correctly on a large line ─────
    #[test]
    fn get_line_text_extracts_from_large_injected_scrollback_line() {
        use crate::grid::Grid;
        use crate::scrollback::{Line, Scrollback};
        // As via checkpoint restore_grid: a tiered scrollback holding one 2 MiB
        // line, twice the scan ceiling.
        let big = vec![b'A'; 2 * 1024 * 1024];
        let mut scrollback = Scrollback::with_defaults();
        scrollback.push_line(Line::from_bytes(&big));
        let grid = Grid::with_tiered_scrollback(4, 80, 1000, scrollback);
        let term = crate::terminal::Terminal::with_grid(grid);

        // In-range extraction (row -1 = most recently scrolled-off) still works and
        // is bounded to the requested column span, not the 2 MiB line.
        let text = term
            .get_line_text(-1, Some((0, 79)))
            .expect("scrollback line has text");
        assert_eq!(text.len(), 80, "extraction bounded to the 80-column range");
        assert!(
            text.bytes().all(|b| b == b'A'),
            "content preserved within the range"
        );
    }

    // ── terminal-relative rows are live-frame (scroll-invariant) ────────────

    /// get_line_text's positive rows are terminal-relative: row 0 is the live
    /// top regardless of scroll position, as the renderer's selection contract
    /// (`sel_row = viewport_row - display_offset`) assumes.
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

    /// Copy reads the rows the highlight paints: the copied text does not
    /// change when the viewport scrolls between drag and copy.
    #[test]
    fn selection_copy_is_scroll_invariant() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut term = crate::terminal::Terminal::new(5, 20);
        for i in 0..30 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        // Select live row 2 ("line28") in terminal-relative coordinates.
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

    /// Fixture: a long docker command auto-wrapped across two rows of an
    /// 80-column screen, then a hard-newline line. Row 0 holds the first 80
    /// columns, row 1 the remaining 50 with `is_wrapped`; row 2 is another
    /// logical line.
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

    /// A soft wrap is one logical line and a hard newline ends it: the span the
    /// word/line click gestures select.
    #[test]
    fn logical_line_span_joins_a_soft_wrap_and_stops_at_a_hard_newline() {
        let (term, _) = wrapped_docker_fixture();
        assert!(
            term.row_continues_previous(1),
            "precondition: row 1 must carry the soft-wrap bit"
        );
        assert!(
            !term.row_continues_previous(2),
            "precondition: row 2 follows a HARD newline"
        );
        assert_eq!(
            term.logical_line_span(0),
            (0, 1),
            "clicking the head of a wrapped line must reach its continuation"
        );
        assert_eq!(
            term.logical_line_span(1),
            (0, 1),
            "…and clicking the continuation must reach back to the head"
        );
        assert_eq!(
            term.logical_line_span(2),
            (2, 2),
            "a hard newline must bound the span — the wrapped line above must \
             not be swallowed, nor this line by it"
        );
    }

    /// The joined text is column-aligned: `first_row + g / cols` / `g % cols`
    /// lands on the cell that holds column `g`.
    #[test]
    fn logical_line_text_is_column_aligned_across_the_wrap() {
        let (term, cmd) = wrapped_docker_fixture();
        let (text, first, last) = term.logical_line_text(0).expect("row 0 has text");
        assert_eq!((first, last), (0, 1));
        assert_eq!(text, cmd, "the joined line must be the whole command");
        // Every non-blank column of the joined line equals the cell it maps to
        // (a single-cell extraction trims its own trailing blank).
        for (g, ch) in cmd.char_indices().filter(|&(_, c)| c != ' ') {
            let row = first + i32::try_from(g / 80).expect("small");
            let col = u16::try_from(g % 80).expect("small");
            let cell = term
                .get_line_text(row, Some((col, col)))
                .expect("cell text");
            assert_eq!(
                cell,
                ch.to_string(),
                "column {g} of the joined line must map back to cell ({row}, {col})"
            );
        }
        assert_eq!(
            term.logical_line_text(2).map(|(t, f, l)| (t, f, l)),
            Some(("AFTER".to_string(), 2, 2)),
            "an unwrapped row is its own logical line, text unchanged"
        );
    }

    /// A logical line that straddles the scrollback/live boundary joins across
    /// it: the head is history row -1, the tail is live row 0.
    #[test]
    fn logical_line_span_joins_across_the_scrollback_boundary() {
        let mut term = crate::terminal::Terminal::new(2, 10);
        // Wraps to two rows, then one more line pushes the head into history.
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
            term.logical_line_text(-1).map(|(t, ..)| t).as_deref(),
            Some("ABCDEFGHIJKL"),
            "and join the two history rows into the whole logical line"
        );
    }
}
