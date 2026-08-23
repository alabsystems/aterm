// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text selection to string conversion for Terminal.
//!
//! Contains `selection_to_string()` and `get_line_text()`.
//! Extracted from mod.rs to reduce file size.

use super::Terminal;
use super::content::visible_row_bounds_to_string;

/// Absolute cap on the number of rows [`Terminal::selection_to_string`] will
/// materialize in one call.
///
/// The row span is already clamped to real content, but "real content" is itself
/// unbounded: scrollback retention is `Option<usize>` where `None` means UNLIMITED
/// (the GUI exposes "0 → unlimited"), so a long-lived terminal can accumulate
/// arbitrarily many history lines — past `i32::MAX` in principle. Without an
/// absolute cap, a control-socket `select`-all + `copy`/`selection` would do
/// O(scrollback) work and build an O(scrollback) `String` while holding the
/// terminal mutex — a UI-freeze DoS reachable by any same-uid process. This bounds
/// the loop to a fixed number of iterations regardless of the anchor values or
/// scrollback magnitude. 1M is ~10× the default 100k retention
/// (`aterm_scrollback` `DEFAULT_LINE_LIMIT`) and the same order as search's
/// `MAX_SEARCH_MATCHES`, so it never truncates a realistic selection; it is a
/// deliberate copy-size bound (an honest hard limit even if more real data exists).
const MAX_SELECTION_ROWS: usize = 1_000_000;

/// Absolute cap on the byte length [`Terminal::selection_to_string`] will produce.
///
/// The binding MEMORY bound: a row cap alone does not bound bytes tightly, since a
/// single stored line can be up to `MAX_GRID_COLS` (4096) wide grapheme clusters of
/// multi-byte text. 64 MiB is ~800k lines at 80 chars and comfortably exceeds a
/// full default 100k-line scrollback (~20 MiB at ~200 bytes/line), so it never
/// clips a realistic copy while capping the pathological one.
const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;

/// Per-line byte ceiling for scrollback text extraction in [`Terminal::get_line_text`].
///
/// A scrollback [`Line`](aterm_scrollback::Line)'s byte length is UNBOUNDED: a
/// crafted checkpoint restored via `restore_grid` -> `deserialize_lines` (which
/// enforces no per-line content cap) can inject a multi-MiB `Line`. Materializing
/// the whole line (the old `Line::to_string()`) would then allocate — and the
/// grapheme walk would scan — the entire line before any column slice or the
/// [`MAX_SELECTION_BYTES`] cap could fire, a memory-amplification DoS reachable by
/// any same-uid producer of the checkpoint. This ceiling bounds BOTH the copy and
/// the walk per line. A legitimate line is at most `MAX_GRID_COLS` (4096) columns,
/// each cell at most 1 base + `Extra::MAX_COMBINING` (16) marks of ≤4-byte scalars
/// (~272 KiB worst case), so 1 MiB never clips real content while capping the
/// pathological injected line.
pub(crate) const MAX_SCROLLBACK_LINE_SCAN_BYTES: usize = 1024 * 1024;

/// Materialize a stored line's FULL text (all columns, no trim) bounded to
/// `max_scan_bytes` — the [`Line::to_string`](aterm_scrollback::Line) equivalent
/// (`from_utf8_lossy` of the bytes) that cannot be blown up by an oversized
/// injected line. Used by the search-index build, whose history rows would
/// otherwise materialize every `Line` whole. Borrows valid UTF-8 under the ceiling
/// zero-copy; lossy-converts only a bounded prefix otherwise.
pub(crate) fn line_text_bounded(bytes: &[u8], max_scan_bytes: usize) -> String {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    String::from_utf8_lossy(scan).into_owned()
}

/// Single-pass column range to byte offset conversion (#5581).
///
/// Walks graphemes once and returns byte offsets for both the start and end
/// columns, replacing the 5 sequential O(n) scans in the scrollback path.
///
/// - `start_col`: first column (inclusive)
/// - `end_col`: last column (inclusive); the returned end byte is one past this grapheme
///
/// Returns `(start_byte, end_byte)` suitable for `&s[start_byte..end_byte]`.
/// If `start_col` is past all content, returns `(s.len(), s.len())`.
fn column_range_to_byte_offsets(s: &str, start_col: usize, end_col: usize) -> (usize, usize) {
    use crate::grapheme::split_graphemes;

    let mut current_col = 0usize;
    let mut start_byte = s.len();
    let mut end_byte = s.len();
    let mut found_start = false;

    for g in split_graphemes(s) {
        let width = g.width;
        if width > 0 {
            let next_col = current_col + width;
            if !found_start && start_col < next_col {
                start_byte = g.byte_offset;
                found_start = true;
            }
            if next_col > end_col {
                end_byte = g.byte_offset + g.text.len();
                break;
            }
            current_col = next_col;
        }
    }

    (start_byte, end_byte)
}

/// Extract a column range from a scrollback line's raw bytes WITHOUT materializing
/// the whole line.
///
/// Bounds BOTH the copy and the grapheme walk to `max_scan_bytes` (in production,
/// [`MAX_SCROLLBACK_LINE_SCAN_BYTES`]): `from_utf8_lossy` borrows the common
/// valid-UTF-8 line under the ceiling zero-copy, and lossy-converts only a bounded
/// prefix for a pathological oversized line. Columns past the scanned content
/// yield an empty string. Split out from [`Terminal::get_line_text`] so the
/// ceiling is unit-testable at a small value (the production ceiling is far above
/// any `u16` column * max grapheme byte width, so it never clips a real request).
fn scrollback_line_range_text(
    bytes: &[u8],
    start_col: u16,
    end_col: u16,
    max_scan_bytes: usize,
) -> String {
    let scan = if bytes.len() <= max_scan_bytes {
        bytes
    } else {
        &bytes[..max_scan_bytes]
    };
    let line_lossy = String::from_utf8_lossy(scan);
    let line: &str = &line_lossy;
    let (start_byte, end_byte) =
        column_range_to_byte_offsets(line, usize::from(start_col), usize::from(end_col));
    if start_byte < line.len() {
        line[start_byte..end_byte].trim_end().to_string()
    } else {
        String::new()
    }
}

/// Append `row_text` to `result` without letting it exceed `max_bytes`, returning
/// `true` if the row was CLAMPED (only the char-boundary-safe prefix that fits was
/// pushed).
///
/// This makes [`MAX_SELECTION_BYTES`] a HARD byte cap even for a single oversized
/// row, not just the per-row soft check at the loop head. A restored/injected
/// visible row can be up to `MAX_GRID_COLS` * `MAX_GRAPHEME_UNIT_BYTES` (~1 MiB)
/// wide, so without this a lone huge row could blow past the cap in one append and
/// leave `truncated` unset. Legitimate selections are far under the cap, so this
/// never clamps real content.
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

/// The row/column span one selection walk needs, computed ONCE by
/// [`Terminal::selection_to_string_capped`] and threaded to whichever shape walk
/// runs.
///
/// Passed as one value rather than re-derived per shape: the block walk and the
/// linear walk share only the caps and the output buffer, so a geometry computed
/// independently in each is exactly the thing that could silently drift.
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
    /// `grid.rows()` widened to the row coordinate type.
    visible_rows: i32,
    /// `grid.cols()`; the caller has already rejected a zero-column grid.
    cols: u16,
}

impl Terminal {
    /// Get the selected text as a string.
    ///
    /// Returns `None` if there is no selection or if the selection is empty.
    /// For block selections, each row is separated by a newline.
    ///
    /// Output is bounded by [`MAX_SELECTION_ROWS`] / [`MAX_SELECTION_BYTES`] so a
    /// pathological selection (e.g. a control-socket `select`-all over an unlimited
    /// scrollback) cannot freeze the UI by working/allocating unboundedly under the
    /// terminal mutex. When those caps clip the selection, the excess is dropped
    /// silently here; use [`Self::selection_to_string_bounded`] when the caller
    /// needs to know whether truncation occurred (e.g. to report it honestly).
    #[must_use]
    pub fn selection_to_string(&self) -> Option<String> {
        self.selection_to_string_bounded().0
    }

    /// Like [`Self::selection_to_string`], but also returns whether content is
    /// MISSING from the returned text.
    ///
    /// The bool lets a caller surface truncation to its client honestly — mirroring
    /// how search reports an `incomplete` result — rather than returning a short
    /// string that is indistinguishable from an exact selection. `true` means real
    /// selected content is absent from EITHER end: the copy caps
    /// ([`MAX_SELECTION_ROWS`] / [`MAX_SELECTION_BYTES`]) drop it from the tail, and
    /// a scrollback eviction that clamped the selection's head to the history floor
    /// drops it from the front.
    #[must_use]
    pub fn selection_to_string_bounded(&self) -> (Option<String>, bool) {
        self.selection_to_string_capped(MAX_SELECTION_ROWS, MAX_SELECTION_BYTES)
    }

    /// [`Self::selection_to_string`] with explicit work/output caps, returning the
    /// text plus a `truncated` flag. Split out so the truncation logic is
    /// unit-testable at small cap values (the production caps are far too large to
    /// exercise directly in a fast test).
    fn selection_to_string_capped(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> (Option<String>, bool) {
        use crate::selection::SelectionType;

        // Use side-adjusted bounds so that the copied text matches the visual
        // highlight. Without this, a Right-sided start or Left-sided end would
        // include an extra character that isn't part of the rendered selection.
        // SELECTION CUSTODY Phase 4: `truncated` is a two-sided report, and the
        // eviction half is already decided before the walk runs. Fold it in HERE too,
        // because head-clamping makes this arm newly reachable: when the surviving
        // anchor also lands on the floor row at col 0 side Left, `apply_side_adjustment`
        // retreats the end to `(min_row - 1, u16::MAX)` and the span collapses. Returning
        // `(None, false)` there would answer `OK 0` with no ` incomplete` — a SILENT
        // total loss, strictly worse than the honest clear this replaced.
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

        // Two-layer bound so no selection — however pathological — can freeze the
        // UI by spinning / allocating unboundedly under the terminal mutex.
        //
        // Layer 1 (span clamp): clamp the row span to the grid's valid coordinate
        // range [-scrollback_lines, rows-1]. Legitimate selections (mouse/vi) already
        // lie within it (no-op, output-preserving); it kills the spurious spin where a
        // control-socket `select` with unclamped i32 anchors would iterate ~2^31 rows
        // that hold no content (get_line_text returns None). `unwrap_or(i32::MAX)` here
        // is never LARGER than real content, so every iterated row addresses real data.
        //
        // Layer 2 (absolute caps, enforced in the loops below): "real content" is
        // itself unbounded (unlimited scrollback can exceed i32::MAX), so the span
        // clamp alone does not bound the work — a genuine select-all over a huge
        // scrollback still does O(scrollback) iterations and builds an O(scrollback)
        // String. MAX_SELECTION_ROWS / MAX_SELECTION_BYTES break the loop after a fixed
        // amount of work/output regardless of the anchors or scrollback size.
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
            visible_rows,
            cols,
        };

        // Iteration runs oldest→newest (first_row→last_row), so a cap truncates the
        // TAIL: the start row and its adj_start_col are always preserved. Each shape
        // walk owns its own counters and hands them back so the cap report below is
        // identical whichever shape ran.
        let (rows_emitted, truncated) = match self.text_selection.selection_type() {
            SelectionType::Block => {
                self.push_block_selection(&mut result, geom, max_rows, max_bytes)
            }
            // Simple, Semantic, Lines, and future variants all use linear selection
            _ => self.push_linear_selection(&mut result, geom, max_rows, max_bytes),
        };

        if truncated {
            // Honest report at the edge (data-layer bound fired). The `truncated`
            // flag is ALSO returned so a caller can surface it machine-readably
            // (cmd_selection/cmd_copy append ` incomplete`, mirroring cmd_search);
            // the warn covers callers that only want the opaque text (mouse, wasm).
            aterm_log::warn!(
                "selection_to_string truncated at cap (rows_emitted={rows_emitted}, bytes={})",
                result.len()
            );
        }

        // The two halves of the report are independent — a capped walk over an
        // already-clamped selection is missing text at both ends — so they OR.
        if result.is_empty() {
            (None, truncated || evicted)
        } else {
            (Some(result), truncated || evicted)
        }
    }

    /// Walk a BLOCK (rectangular) selection into `result`, returning
    /// `(rows_emitted, truncated)`.
    ///
    /// Split out of [`Self::selection_to_string_capped`] purely so each selection
    /// shape is its own function; the walk is unchanged. `geom` carries the row/col
    /// span the caller already computed — the two shape walks share only the caps
    /// and the output buffer, so nothing here re-derives geometry.
    ///
    /// The walk body moved verbatim except for one forced token: `&mut result`
    /// became `result`, the same reborrow now that the buffer arrives as a
    /// reference (`clippy::needless_borrow`).
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
                if let Some(line) = self.get_line_text(row, Some((adj_start_col, adj_end_col)))
                {
                    if push_row_capped(result, &line, max_bytes) {
                        truncated = true;
                        break;
                    }
                }
            }

        (rows_emitted, truncated)
    }

    /// Walk a LINEAR (Simple/Semantic/Lines) selection into `result`, returning
    /// `(rows_emitted, truncated)`.
    ///
    /// Split out of [`Self::selection_to_string_capped`] alongside
    /// [`Self::push_block_selection`]; the walk — including the SCR-4
    /// one-fetch-per-history-row hoist described inside — is unchanged, save the
    /// same `&mut result` -> `result` reborrow noted there.
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
            visible_rows,
            cols,
        } = geom;
        let mut rows_emitted = 0usize;
        let mut truncated = false;

            for row in first_row..=last_row {
                if rows_emitted >= max_rows || result.len() >= max_bytes {
                    truncated = true;
                    break;
                }
                rows_emitted += 1;
                // ONE tier fetch per HISTORY row (SCR-4). This loop used to
                // resolve every scrollback row TWICE — once below for the
                // soft-wrap bit and once inside `get_line_text` for the text
                // — i.e. 2N-1 tier reads for an N-row selection, where each
                // read is a full `Line` construction on the ring path
                // (Row -> String + RLE attrs + hyperlink clone) or a `Line`
                // CLONE out of the decompressed block on warm/cold. The two
                // uses are hoisted onto one `Cow<Line>` here; both leaves
                // below read it instead of re-entering the tiers.
                //
                // Behaviour is identical by construction: the wrap flag is
                // the same `Line::is_wrapped()` off the same line, and the
                // text branch inlines exactly what `get_line_text`'s
                // negative-row arm does with the same column range and the
                // same `MAX_SCROLLBACK_LINE_SCAN_BYTES` ceiling. Live rows
                // (`row >= 0`) keep going through `get_line_text` unchanged
                // — they never touched the tiers to begin with.
                let history_line = if row < 0 {
                    usize::try_from(-(i64::from(row)) - 1)
                        .ok()
                        .and_then(|rev_idx| self.grid.history_line_rev(rev_idx))
                } else {
                    None
                };
                if row > first_row {
                    // Only insert newline if this row is NOT a soft-wrap continuation.
                    // Row::is_wrapped() / Line::is_wrapped() means "this row continues
                    // the previous row's content" (soft wrap, not a hard line break).
                    #[allow(
                        clippy::redundant_closure_for_method_calls,
                        reason = "private row/line types prevent method-reference shorthand"
                    )]
                    let is_continuation = if row >= 0 && row < visible_rows {
                        // LIVE-frame read: selection rows are terminal-relative
                        // (see get_line_text), so the display-mapped Grid::row
                        // would test the wrong row's wrap flag while scrolled.
                        u16::try_from(row)
                            .ok()
                            .and_then(|idx| self.grid.row_at_screen(idx))
                            .is_some_and(|r| r.is_wrapped())
                    } else if row < 0 {
                        history_line.as_ref().is_some_and(|l| l.is_wrapped())
                    } else {
                        false
                    };
                    if !is_continuation {
                        result.push('\n');
                    }
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

                let line = if row < 0 {
                    history_line.as_ref().map(|stored| {
                        scrollback_line_range_text(
                            stored.as_bytes(),
                            start_col,
                            end_col,
                            MAX_SCROLLBACK_LINE_SCAN_BYTES,
                        )
                    })
                } else {
                    self.get_line_text(row, Some((start_col, end_col)))
                };
                if let Some(line) = line {
                    if push_row_capped(result, &line, max_bytes) {
                        truncated = true;
                        break;
                    }
                }
            }

        (rows_emitted, truncated)
    }

    /// Get text from a line (visible or scrollback).
    ///
    /// `row` is TERMINAL-relative and scroll-invariant: `0..rows` are the LIVE
    /// screen rows (read offset-independently — the renderer paints a
    /// selection at `viewport_row - display_offset`, so these must not follow
    /// the scroll position), negative rows are scrollback (`-1` = the newest
    /// history line). `col_range` specifies the column range to extract
    /// (inclusive). If `None`, extracts the entire line.
    pub fn get_line_text(&self, row: i32, col_range: Option<(u16, u16)>) -> Option<String> {
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
                &self.grid, row_idx, start_col, end_col,
            ))
        } else if row < 0 {
            // Scrollback row (negative indices)
            // history_line_rev provides unified access to both ring buffer
            // and tiered scrollback: rev_idx 0 = most recently scrolled-off line.
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            if let Some(scrollback_line) = self.grid.history_line_rev(rev_idx) {
                let cols = self.grid.cols();
                if cols == 0 && col_range.is_none() {
                    return Some(String::new());
                }
                let (start_col, end_col) = col_range.unwrap_or((0, cols.saturating_sub(1)));

                // Extract WITHOUT materializing the whole line. A stored line's byte
                // length is unbounded (a crafted checkpoint can inject a multi-MiB
                // Line — see MAX_SCROLLBACK_LINE_SCAN_BYTES), so the old
                // `Line::to_string()` allocated, and the grapheme walk scanned, the
                // entire line before any column slice or selection cap could fire.
                return Some(scrollback_line_range_text(
                    scrollback_line.as_bytes(),
                    start_col,
                    end_col,
                    MAX_SCROLLBACK_LINE_SCAN_BYTES,
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
    use super::column_range_to_byte_offsets;

    /// SCR-4, pinned two ways: the copy is byte-identical to what the
    /// double-fetch loop produced, AND every selected history row is resolved
    /// from the tier EXACTLY ONCE.
    ///
    /// The count is the load-bearing half. The linear branch used to fetch each
    /// scrollback row twice — once for the soft-wrap bit, once for the text —
    /// so an N-row selection cost 2N-1 tier reads, each a full `Line`
    /// construction on the ring path. `take_row_to_line_ops` counts exactly
    /// those constructions (`row_to_line_with_stored_extras`), so this asserts
    /// N, not 2N-1, and fails loudly if the second fetch ever comes back.
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
        // Ring reads/writes the FIXTURE performed are not what is being counted.
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

        // IDENTITY FIRST: the hoist must not move one byte of the copy. History
        // holds fill lines 0..=(FILL - ROWS), so row -1 is fill line
        // FILL - ROWS and row -SELECTED is SELECTED-1 lines above it.
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

    #[test]
    fn ascii_full_range() {
        // "Hello" cols 0..4 → all 5 chars
        let (s, e) = column_range_to_byte_offsets("Hello", 0, 4);
        assert_eq!(&"Hello"[s..e], "Hello");
    }

    #[test]
    fn ascii_sub_range() {
        // "Hello World" cols 0..4 → "Hello"
        let (s, e) = column_range_to_byte_offsets("Hello World", 0, 4);
        assert_eq!(&"Hello World"[s..e], "Hello");
    }

    #[test]
    fn ascii_mid_range() {
        // "Hello World" cols 6..10 → "World"
        let (s, e) = column_range_to_byte_offsets("Hello World", 6, 10);
        assert_eq!(&"Hello World"[s..e], "World");
    }

    #[test]
    fn wide_char_single() {
        // "你好" — each CJK char is width 2: cols 0..1 → "你"
        let (s, e) = column_range_to_byte_offsets("你好", 0, 1);
        assert_eq!(&"你好"[s..e], "你");
    }

    #[test]
    fn wide_char_both() {
        // "你好" cols 0..3 → "你好" (col 0-1 = 你, col 2-3 = 好)
        let (s, e) = column_range_to_byte_offsets("你好", 0, 3);
        assert_eq!(&"你好"[s..e], "你好");
    }

    #[test]
    fn mixed_ascii_wide() {
        // "A你B" — A=col0, 你=col1-2, B=col3. Extract cols 1..2 → "你"
        let (s, e) = column_range_to_byte_offsets("A你B", 1, 2);
        assert_eq!(&"A你B"[s..e], "你");
    }

    #[test]
    fn start_past_content() {
        let s = "Hi";
        let (start, end) = column_range_to_byte_offsets(s, 10, 20);
        assert_eq!(start, s.len());
        assert_eq!(end, s.len());
    }

    #[test]
    fn empty_string() {
        let (s, e) = column_range_to_byte_offsets("", 0, 5);
        assert_eq!(s, 0);
        assert_eq!(e, 0);
    }

    #[test]
    fn single_column() {
        // "abc" col 1..1 → "b"
        let (s, e) = column_range_to_byte_offsets("abc", 1, 1);
        assert_eq!(&"abc"[s..e], "b");
    }

    // ── selection_to_string absolute caps (rigorous DoS bound) ──────────────
    //
    // The production caps (1M rows / 64 MiB) are too large to hit in a fast test,
    // so these exercise selection_to_string_capped at small cap values. They lock:
    // (1) the row cap truncates the TAIL while preserving the start row/col,
    // (2) the byte cap truncates, and (3) caps above the content are byte-identical
    // to the uncapped public method (no regression for legitimate selections).

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
        // max_bytes smaller than the FIRST row's width: the cap must clamp mid-row
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

    // ── scrollback line extraction bounds the scan to the ceiling ───────────────
    //
    // A crafted checkpoint restored via deserialize_lines (no per-line content cap)
    // can inject a multi-MiB Line. scrollback_line_range_text must NOT materialize
    // or scan the whole line: it borrows at most `max_scan_bytes`. Tested at a small
    // ceiling since the production ceiling is far above any expressible request.
    #[test]
    fn scrollback_line_range_text_bounds_scan_to_ceiling() {
        use super::scrollback_line_range_text;
        // 2000 single-byte, single-width columns; ceiling of 100 bytes.
        let bytes = vec![b'A'; 2000];

        // In-range within the ceiling: cols 0..9 → 10 'A's.
        assert_eq!(scrollback_line_range_text(&bytes, 0, 9, 100).len(), 10);

        // Columns past the 100-byte ceiling are not materialized → empty. This is
        // the regression guard: the old full `Line::to_string()` path ignored any
        // ceiling and WOULD have returned 10 'A's here.
        assert!(
            scrollback_line_range_text(&bytes, 500, 509, 100).is_empty(),
            "columns past the scan ceiling must not be materialized"
        );

        // With a ceiling above the requested column, the SAME request is reachable
        // — proving the emptiness above is the ceiling, not a logic bug.
        assert_eq!(scrollback_line_range_text(&bytes, 500, 509, 4096).len(), 10);
    }

    #[test]
    fn scrollback_line_range_text_lossy_prefix_is_bounded() {
        use super::scrollback_line_range_text;
        // Invalid UTF-8 forces the lossy path; a >ceiling line must still only touch
        // a bounded prefix (no panic, bounded output).
        let mut bytes = vec![0xFFu8; 4096]; // invalid UTF-8, 4096 bytes
        bytes.extend_from_slice(b"tail");
        let out = scrollback_line_range_text(&bytes, 0, 3, 64);
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
        // Mirror the reachable injection path (checkpoint restore_grid): a tiered
        // scrollback holding one pathologically large line — 2 MiB of 'A', far
        // above any legitimate line and twice the 1 MiB scan ceiling.
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

    /// get_line_text's POSITIVE rows are terminal-relative: row 0 is the live
    /// top regardless of the scroll position (negative rows address history).
    /// The renderer's selection contract (`sel_row = viewport_row -
    /// display_offset`) and display_row_text's conversion both assume this;
    /// pre-fix the positive arm read display-mapped cells, so a pure viewport
    /// scroll silently shifted what it returned.
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

    /// Copy must read the SAME rows the selection highlight paints. The
    /// selection stores terminal-relative rows and the renderer subtracts
    /// display_offset when painting, so the copied text must not change when
    /// the viewport scrolls between the drag and the copy.
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
}
