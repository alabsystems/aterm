// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A row as search reads it: its text as written, the grid cells that text was read
//! from, and whether it continues the row above.
//!
//! The search index reports a match in the cells that hold it ([`LineCells`]), not where a
//! walk of the text would put it: the grid folds a character into the cell before it only
//! when that cell is the one before the cursor as it arrives, so two flag halves written
//! out of order (or brought together by a delete-character) stay two cells, and a VS16 base
//! in a row's last column stays one cell wide. Every search over this terminal reads its
//! rows here: the index slot's builds and refreshes and the GUI's index through
//! [`Terminal::read_search_rows`], and the budgeted search.
//!
//! A history row's cells are the ones it is drawn in ([`history_line_text_cells`]): the
//! walk of its stored line, which keeps apart the cells the grid drew apart.
//! The stored walk costs one unit scan per grapheme (about 0.9 µs a row of 70 decomposed
//! accents: +17% on a full build of such scrollback, within noise on ASCII, agent output
//! and CJK); a full build runs only on a cold or invalidated index.
//!
//! Rows are addressed by absolute row, the search index's coordinate: history line `i`
//! (0 = oldest retained) is `oldest_absolute_row() + i`, and live screen row `r` follows
//! the history. Every reader here reads the live frame, never the viewport: a screen row's
//! text and its soft-wrap flag come from the row on the screen whatever `display_offset`
//! the viewport is scrolled back by.

use std::borrow::Cow;

use aterm_scrollback::Line;

use super::Terminal;
use super::content::visible_row_written;
use super::selection::{MAX_SCROLLBACK_LINE_SCAN_BYTES, line_text_bounded};
use crate::grid::{Grid, stored_line_cells};
use crate::search::LineCells;

/// How far back [`Terminal::logical_origin`] looks for the row a soft-wrap run starts
/// on. A run this long is not a wrapped sentence, it is a file with no newlines catted
/// into the grid, and the walk runs once per anchored search, so past this depth the
/// callers take their bounded fallback (rebuild from the retained floor, leave the
/// anchor physical) instead of a scan over the whole retained history.
const MAX_LOGICAL_LINE_WALK: usize = 1024;

/// Rows of the terminal as search reads them ([`Terminal::read_search_rows`]): one entry
/// per row in each vector.
#[derive(Debug, Default)]
pub struct SearchRows {
    /// Each row's text as written, trailing blanks kept: a live row's written cells, a
    /// history row's stored line up to [`MAX_SCROLLBACK_LINE_SCAN_BYTES`].
    pub text: Vec<String>,
    /// The cells each row's text was read from; a history row the read bound cut has
    /// them marked truncated ([`LineCells::is_truncated`]).
    pub cells: Vec<LineCells>,
    /// Whether each row continues the row above it ([`Terminal::abs_row_continues`]).
    pub wrapped: Vec<bool>,
}

impl SearchRows {
    /// Room for `rows` rows.
    #[must_use]
    #[inline]
    pub fn with_capacity(rows: usize) -> Self {
        Self {
            text: Vec::with_capacity(rows),
            cells: Vec::with_capacity(rows),
            wrapped: Vec::with_capacity(rows),
        }
    }

    #[inline]
    fn push(&mut self, (text, cells): (String, LineCells), wrapped: bool) {
        self.text.push(text);
        self.cells.push(cells);
        self.wrapped.push(wrapped);
    }
}

/// The text of an absolute row ([`Terminal::abs_row_text_within`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbsRowText {
    /// The row's text as written, combining marks and clusters intact, control
    /// characters not yet collapsed.
    Text(String),
    /// Older than `oldest_absolute_row()`: scrolled past the scrollback cap.
    Evicted,
    /// Below the live screen's bottom row (no such row).
    OutOfRange,
    /// Within retained history, but its stored line could not be read.
    Unreadable,
}

/// Where an absolute row stands ([`Terminal::abs_row_at`]): what `cell abs=<n>` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbsRowAt {
    /// In the retained history, `rev_idx` lines above the live screen (0 = the newest
    /// scrolled-off line): [`Terminal::render_history_row_into`] resolves its cells.
    History {
        /// Lines above the live screen: 0 is the newest scrolled-off line.
        rev_idx: usize,
    },
    /// On the live screen, at `row`.
    Screen {
        /// The live screen row, 0 at its top.
        row: usize,
    },
    /// Older than `oldest_absolute_row()`: scrolled past the scrollback cap.
    Evicted,
    /// Below the live screen's bottom row (no such row).
    OutOfRange,
}

impl Terminal {
    /// [`written_line_text`](Self::written_line_text) with the grid cells its
    /// text was read from: a visible row's live cells, or the cells a history
    /// row is drawn in ([`history_line_text_cells`]). `None` when `row`
    /// addresses no line.
    #[must_use]
    pub fn written_line_cells(&self, row: i32) -> Option<(String, LineCells)> {
        if row < 0 {
            let rev_idx = usize::try_from(-(i64::from(row)) - 1).ok()?;
            let line = self.grid.history_line_rev(rev_idx)?;
            return Some(history_row_written(&self.grid, &line));
        }
        let row = u16::try_from(row)
            .ok()
            .filter(|&row| row < self.grid.rows())?;
        Some(visible_row_written(&self.grid, row))
    }

    /// Append absolute rows `first..first + count` to `out`, exactly `count` of them:
    /// each row's text and cells as [`written_line_cells`](Self::written_line_cells)
    /// reads them and its soft-wrap flag as [`abs_row_continues`](Self::abs_row_continues)
    /// answers, a row that is evicted, unreadable or past the screen's bottom reading as
    /// an empty, unwrapped one.
    ///
    /// The history part is read through ONE dense walk ([`Grid::history_lines_from`]): one
    /// decode per tiered segment, the text and the wrap flag taken from the same yielded
    /// line, where the per-row readers pay a binary search, a block-cache probe and a
    /// `Line` clone per tiered row. Rows stay keyed by position (an undecodable segment
    /// yields one placeholder per line it spans), so every later row keeps its absolute
    /// row. Text and flags come from the one frame the caller's hold of the terminal
    /// sees, so a layout and its content never come from two frames.
    pub fn read_search_rows(&self, first: u64, count: usize, out: &mut SearchRows) {
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        let history_end =
            oldest.saturating_add(u64::try_from(grid.scrollback_lines()).unwrap_or(u64::MAX));
        let end = first.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let rows_until = |bound: u64, from: u64| {
            usize::try_from(bound.min(end).saturating_sub(from)).unwrap_or(usize::MAX)
        };
        let mut abs = first;
        // Rows scrolled past the cap.
        let evicted = rows_until(oldest, abs);
        for _ in 0..evicted {
            out.push(Default::default(), false);
        }
        abs = abs.saturating_add(u64::try_from(evicted).unwrap_or(u64::MAX));
        // History, through the dense walk. The oldest retained row is never a
        // continuation: its run's head is gone.
        let in_history = rows_until(history_end, abs);
        if in_history > 0 {
            let start = usize::try_from(abs - oldest).unwrap_or(usize::MAX);
            let mut lines = grid.history_lines_from(start);
            for k in 0..in_history {
                let line = lines.next().flatten();
                let wrapped = (start > 0 || k > 0) && line.as_deref().is_some_and(Line::is_wrapped);
                out.push(history_row_for_search(grid, line), wrapped);
            }
            abs = abs.saturating_add(u64::try_from(in_history).unwrap_or(u64::MAX));
        }
        // The live screen, and anything past its bottom. Counted, not bounded by `end`,
        // so exactly `count` rows are appended whatever saturates.
        for _ in evicted.saturating_add(in_history)..count {
            let row = abs
                .checked_sub(history_end)
                .and_then(|visible| i32::try_from(visible).ok());
            out.push(
                row.and_then(|row| self.written_line_cells(row))
                    .unwrap_or_default(),
                self.abs_row_continues(abs),
            );
            abs = abs.saturating_add(1);
        }
    }

    /// Whether absolute row `abs` is a soft-wrap continuation: the grid's destination
    /// convention, where the row content flowed into carries the flag, so the first row
    /// of a logical line reads `false`. The flag selection reads
    /// ([`row_continues_previous`](Self::row_continues_previous)): a history row's stored
    /// line, a screen row's live row whatever the viewport shows.
    ///
    /// A row outside the retained range, and the oldest retained row, are not
    /// continuations: a run whose head has been evicted starts at the oldest row still
    /// there, the only logical line the search index can honestly key.
    #[must_use]
    #[inline]
    pub fn abs_row_continues(&self, abs: u64) -> bool {
        abs > self.grid.oldest_absolute_row()
            && self
                .abs_selection_row(abs)
                .is_some_and(|row| self.row_continues_previous(row))
    }

    /// The absolute row the search index keys the logical line containing `abs` under:
    /// the walk back over soft-wrap continuations ([`abs_row_continues`](Self::abs_row_continues))
    /// to the row the run starts on, stopping at `floor` (the oldest row the index
    /// retains).
    ///
    /// Every column the index reports is measured from THIS row's column 0, so a caller
    /// holding a physical `(row, col)` point (a find anchor) comes through here before it
    /// can address the index at all. `None` when the run reaches back further than
    /// [`MAX_LOGICAL_LINE_WALK`] rows without meeting its head or the floor.
    #[must_use]
    pub fn logical_origin(&self, abs: u64, floor: u64) -> Option<u64> {
        let mut origin = abs;
        for _ in 0..MAX_LOGICAL_LINE_WALK {
            if origin <= floor || !self.abs_row_continues(origin) {
                return Some(origin);
            }
            origin -= 1;
        }
        None
    }

    /// Whether absolute row `abs` carries the soft-wrap continuation flag, the oldest
    /// retained row included ([`abs_row_continues`](Self::abs_row_continues) reads that
    /// one as a line's start): what an index joined it as while older rows were still
    /// retained.
    #[inline]
    pub(crate) fn abs_row_wrap_flag(&self, abs: u64) -> bool {
        self.abs_selection_row(abs)
            .is_some_and(|row| self.row_continues_previous(row))
    }

    /// The exclusive end of the soft-wrap run absolute row `abs` belongs to: the first
    /// row after `abs` that is not a continuation, walking no further than `stop`, and
    /// `stop` when the run reaches it or runs on past [`MAX_LOGICAL_LINE_WALK`] rows.
    pub(crate) fn logical_run_end(&self, abs: u64, stop: u64) -> u64 {
        let mut end = abs.saturating_add(1);
        for _ in 0..MAX_LOGICAL_LINE_WALK {
            if end >= stop || !self.abs_row_continues(end) {
                return end.min(stop);
            }
            end = end.saturating_add(1);
        }
        stop
    }

    /// Text of absolute row `abs` as written, trailing blanks kept: history line
    /// `abs - oldest_absolute_row()` read up to `max_bytes` of its stored bytes
    /// ([`MAX_SCROLLBACK_LINE_SCAN_BYTES`] as search reads it, `usize::MAX` to read it
    /// whole), else live screen row `abs - oldest - scrollback_lines` through the
    /// combining-aware [`written_line_text`](Self::written_line_text), so accents and ZWJ
    /// clusters survive and find answers the same for a row on the screen and after it
    /// scrolls into history.
    #[must_use]
    pub fn abs_row_text_within(&self, abs: u64, max_bytes: usize) -> AbsRowText {
        let grid = &self.grid;
        let Some(rel) = abs.checked_sub(grid.oldest_absolute_row()) else {
            return AbsRowText::Evicted;
        };
        let Ok(rel) = usize::try_from(rel) else {
            return AbsRowText::OutOfRange;
        };
        let scrollback = grid.scrollback_lines();
        if rel < scrollback {
            return grid
                .get_history_line(rel)
                .map_or(AbsRowText::Unreadable, |line| {
                    AbsRowText::Text(line_text_bounded(line.as_bytes(), max_bytes).into_owned())
                });
        }
        let visible = rel - scrollback;
        if visible >= usize::from(self.rows()) {
            return AbsRowText::OutOfRange;
        }
        AbsRowText::Text(
            i32::try_from(visible)
                .ok()
                .and_then(|row| self.written_line_text(row))
                .unwrap_or_default(),
        )
    }

    /// Where absolute row `abs` (B-2's one coordinate space, `line`'s and `search`'s) stands:
    /// a history row, by how far above the live screen, or a live screen row — the read
    /// [`abs_row_text_within`](Self::abs_row_text_within) makes, for a reader of the row's
    /// cells (`cell abs=<n>`).
    #[must_use]
    pub fn abs_row_at(&self, abs: u64) -> AbsRowAt {
        let grid = &self.grid;
        let Some(rel) = abs.checked_sub(grid.oldest_absolute_row()) else {
            return AbsRowAt::Evicted;
        };
        let Ok(rel) = usize::try_from(rel) else {
            return AbsRowAt::OutOfRange;
        };
        let scrollback = grid.scrollback_lines();
        if rel < scrollback {
            return AbsRowAt::History {
                rev_idx: scrollback - 1 - rel,
            };
        }
        let row = rel - scrollback;
        if row >= usize::from(self.rows()) {
            return AbsRowAt::OutOfRange;
        }
        AbsRowAt::Screen { row }
    }

    /// Text of absolute row `abs` as search reads it
    /// ([`abs_row_text_within`](Self::abs_row_text_within) at
    /// [`MAX_SCROLLBACK_LINE_SCAN_BYTES`]), so a snippet equals the line the match came
    /// from. `None` when `abs` is evicted, unreadable or past the live screen. Feeds
    /// `search_summary`'s snippet.
    #[must_use]
    #[inline]
    pub fn abs_row_text(&self, abs: u64) -> Option<String> {
        match self.abs_row_text_within(abs, MAX_SCROLLBACK_LINE_SCAN_BYTES) {
            AbsRowText::Text(text) => Some(text),
            AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => None,
        }
    }

    /// The selection-space row of absolute row `abs` (negative in history, `0..rows` on
    /// the live screen), `None` when `abs` is evicted or the row does not fit.
    #[inline]
    fn abs_selection_row(&self, abs: u64) -> Option<i32> {
        let rel = i128::from(abs.checked_sub(self.grid.oldest_absolute_row())?);
        let scrollback = i128::try_from(self.grid.scrollback_lines()).ok()?;
        i32::try_from(rel - scrollback).ok()
    }
}

/// A history line read for search from the dense history walk
/// (`Grid::history_lines_from`): its text and cells, or an empty row for a line
/// the walk could not read, so later rows keep their absolute rows.
#[inline]
pub(crate) fn history_row_for_search(
    grid: &Grid,
    line: Option<Cow<'_, Line>>,
) -> (String, LineCells) {
    line.map(|line| history_row_written(grid, &line))
        .unwrap_or_default()
}

/// A history row whose stored line is `line`, as search reads it: its text as
/// written, bounded by [`MAX_SCROLLBACK_LINE_SCAN_BYTES`] as
/// [`Terminal::written_line_text`] reads it, with the cells that text is drawn
/// in. A line the bound cuts has its cells marked ([`LineCells::mark_truncated`]),
/// so the index counts it as a line it did not search to its end.
#[inline]
fn history_row_written(grid: &Grid, line: &Line) -> (String, LineCells) {
    let bytes = line.as_bytes();
    let text = line_text_bounded(bytes, MAX_SCROLLBACK_LINE_SCAN_BYTES).into_owned();
    let mut cells = history_line_text_cells(grid, line, &text);
    if bytes.len() > MAX_SCROLLBACK_LINE_SCAN_BYTES {
        cells.mark_truncated();
    }
    (text, cells)
}

/// The cells `text`, a read of a history row of `grid` whose stored line is `line`, is
/// drawn in: the walk of the stored line ([`stored_line_cells`]), which lays the row out on
/// every history tier, the ring's own fast path included, and keeps apart the cells the
/// grid drew apart. Every reader of a history row's cells comes here (search, a
/// double-click's window, a web host's word under a cell, a copy's columns), so a column
/// names the cell drawn there.
#[must_use]
#[inline]
pub(crate) fn history_line_text_cells(grid: &Grid, line: &Line, text: &str) -> LineCells {
    stored_line_text_cells(line, text, grid.cols())
}

/// The cells `text`, a read of stored history `line`, is drawn in on a row
/// `cols` wide: the walk ([`stored_line_cells`]) the row is laid out by.
#[inline]
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

#[cfg(test)]
mod tests {
    use aterm_grid::Row;
    use aterm_scrollback::{Line, Scrollback};

    use super::{AbsRowText, SearchRows};
    use crate::grid::Grid;
    use crate::search::LineCells;
    use crate::terminal::Terminal;
    use crate::terminal::selection::MAX_SCROLLBACK_LINE_SCAN_BYTES;

    /// A terminal whose history runs through a TIERED store (a five-row ring over it)
    /// holding a corrupt warm block between its cold tier and its healthy warm blocks,
    /// a needle on each side of it, and soft-wrapped lines in the ring and tiered
    /// history and across the history/screen edge: the shapes a skipping reader, or
    /// one that took the wrap flag from a different row than the text, gets wrong.
    fn corrupt_tiered_terminal() -> (Terminal, usize) {
        let (mut t, corrupt) = crate::terminal::search_index::tests::corrupt_tiered_terminal();
        for i in 0..20 {
            if i % 4 == 1 {
                // 95 columns into a 40-column grid: one head row, two continuation rows.
                t.process(format!("wrapped {i:03} {}\r\n", "w".repeat(83)).as_bytes());
            } else {
                t.process(format!("live {i:03}\r\n").as_bytes());
            }
        }
        // End on a run whose head has scrolled into history and whose seven rows'
        // tail fills the screen: it straddles the history/screen edge.
        t.process(format!("edge {}", "e".repeat(250)).as_bytes());
        assert_eq!(
            t.grid()
                .history_lines_from(0)
                .filter(Option::is_none)
                .count(),
            corrupt,
            "the fixture's corrupt block is still in the store"
        );
        (t, corrupt)
    }

    /// A ring-only terminal whose history has been capped, so the oldest absolute rows
    /// are EVICTED and the oldest retained row is a soft-wrap continuation whose head
    /// is gone.
    fn evicted_terminal() -> Terminal {
        let mut t = Terminal::new(4, 20);
        t.set_scrollback_line_limit(Some(9));
        // Every line is two rows: a head and one continuation.
        for i in 0..30 {
            t.process(format!("row {i:02} {}\r\n", "r".repeat(30)).as_bytes());
        }
        if !oldest_is_a_continuation(&t) {
            let keep = t.grid().scrollback_lines() - 1;
            t.set_scrollback_line_limit(Some(keep));
        }
        assert!(t.grid().oldest_absolute_row() > 0, "rows were evicted");
        assert!(
            oldest_is_a_continuation(&t),
            "the oldest retained row is a continuation whose head was evicted"
        );
        t
    }

    fn oldest_is_a_continuation(t: &Terminal) -> bool {
        t.grid()
            .history_lines_from(0)
            .next()
            .flatten()
            .is_some_and(|line| line.is_wrapped())
    }

    type Read = (Vec<String>, Vec<LineCells>, Vec<bool>);

    /// The per-row oracle: what `abs_row_text_within` (at the search bound),
    /// `written_line_cells` and `abs_row_continues` answer row by row.
    fn per_row(t: &Terminal, first: u64, count: usize) -> Read {
        let oldest = i128::from(t.grid().oldest_absolute_row());
        let scrollback = t.grid().scrollback_lines() as i128;
        let mut read: Read = Default::default();
        for abs in first..first + count as u64 {
            read.0.push(
                match t.abs_row_text_within(abs, MAX_SCROLLBACK_LINE_SCAN_BYTES) {
                    AbsRowText::Text(text) => text,
                    AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => {
                        String::new()
                    }
                },
            );
            let row = i128::from(abs) - oldest - scrollback;
            read.1.push(
                (i128::from(abs) >= oldest)
                    .then(|| i32::try_from(row).ok())
                    .flatten()
                    .and_then(|row| t.written_line_cells(row))
                    .map(|(_, cells)| cells)
                    .unwrap_or_default(),
            );
            read.2.push(t.abs_row_continues(abs));
        }
        read
    }

    fn dense(t: &Terminal, first: u64, count: usize) -> Read {
        let mut rows = SearchRows::default();
        t.read_search_rows(first, count, &mut rows);
        (rows.text, rows.cells, rows.wrapped)
    }

    /// Every window `first..first + count` over evicted rows, the whole retained
    /// history, the screen and past its bottom.
    fn assert_matches_everywhere(t: &Terminal) {
        let oldest = t.grid().oldest_absolute_row();
        let bottom = oldest + t.grid().scrollback_lines() as u64 + u64::from(t.rows());
        let lo = oldest.saturating_sub(3);
        for first in lo..=bottom + 2 {
            for count in 0..=usize::try_from(bottom + 3 - first).expect("small") {
                assert_eq!(
                    dense(t, first, count),
                    per_row(t, first, count),
                    "rows {first}..{} (oldest {oldest})",
                    first + count as u64
                );
            }
        }
    }

    /// The whole retained window, read at the live tail, then scrolled back by each of
    /// `offsets`: every read equals the live one and the per-row readers.
    fn assert_scroll_invariant(t: &mut Terminal, offsets: &[i32]) {
        let oldest = t.grid().oldest_absolute_row();
        let total = t.grid().scrollback_lines() + usize::from(t.rows());
        assert_eq!(
            t.grid().display_offset(),
            0,
            "precondition: at the live tail"
        );
        let live = dense(t, oldest, total);
        for &offset in offsets {
            t.scroll_display(offset);
            assert_eq!(
                t.grid().display_offset(),
                usize::try_from(offset).expect("non-negative"),
                "precondition: scrolled back"
            );
            assert_eq!(dense(t, oldest, total), live, "scrolled back {offset}");
            assert_matches_everywhere(t);
            t.scroll_display(-offset);
        }
    }

    #[test]
    fn search_rows_read_matches_the_per_row_readers_over_a_corrupt_tiered_segment() {
        let (mut t, corrupt) = corrupt_tiered_terminal();
        assert_matches_everywhere(&t);
        let oldest = t.grid().oldest_absolute_row();
        let total = t.grid().scrollback_lines() + usize::from(t.rows());
        let (text, _, wrapped) = dense(&t, oldest, total);
        let scrollback = t.grid().scrollback_lines();
        assert!(
            wrapped[..scrollback].iter().any(|&w| w),
            "history holds soft wraps"
        );
        assert!(
            wrapped[scrollback] && wrapped[total - 1],
            "the last run straddles the history/screen edge"
        );
        // NEGATIVE CONTROL: a reader that skips the undecodable segment (the shape
        // `ScrollbackIter` has) moves every later row onto its neighbour's coordinate,
        // which the conformance above would catch.
        let skipping: Vec<String> = t
            .grid()
            .history_lines_from(0)
            .flatten()
            .map(|line| line.to_string())
            .collect();
        let after = |rows: &[String]| {
            rows.iter()
                .position(|row| row.starts_with("NEEDLE_after"))
                .expect("the second needle is read")
        };
        assert_eq!(after(&skipping) + corrupt, after(&text));
        // Scrolled back inside the ring, and into the tiered history past it.
        assert_scroll_invariant(&mut t, &[2, 30]);
    }

    #[test]
    fn search_rows_read_matches_the_per_row_readers_over_evicted_history() {
        let mut t = evicted_terminal();
        assert_matches_everywhere(&t);
        let oldest = t.grid().oldest_absolute_row();
        let (_, _, wrapped) = dense(&t, oldest, 1);
        assert_eq!(wrapped, [false], "which reads as the start of a line");
        assert_scroll_invariant(&mut t, &[1, 5]);
    }

    /// A screen row's soft-wrap flag is the live row's, whatever the viewport shows:
    /// scrolled back inside the ring or into the tiered history past it, every screen
    /// row reads the flag it reads at the live tail.
    #[test]
    fn scrolled_back_wrap_bits_are_live() {
        // A ten-row ring over tiered history, and a screen of lines one, two and three
        // rows long, so no shift of the flags lines them up again.
        let mut t = Terminal::with_scrollback(6, 20, 10, Scrollback::with_defaults());
        for i in 0..40 {
            t.process(format!("{i:02} {}\r\n", "x".repeat((i % 3) * 20)).as_bytes());
        }
        t.process(format!("tail {}", "t".repeat(30)).as_bytes());
        let rows = t.rows();
        let screen = t.grid().oldest_absolute_row() + t.grid().scrollback_lines() as u64;
        let read = |t: &Terminal| -> Vec<bool> {
            (screen..screen + u64::from(rows))
                .map(|abs| t.abs_row_continues(abs))
                .collect()
        };
        let live = read(&t);
        assert!(live.iter().any(|&w| w), "the screen holds soft wraps");
        for offset in [1, 3, 30] {
            t.scroll_display(offset);
            assert_eq!(
                t.grid().display_offset(),
                usize::try_from(offset).expect("non-negative"),
                "precondition: scrolled back"
            );
            assert_eq!(read(&t), live, "scrolled back {offset}");
            // NEGATIVE CONTROL: the viewport accessor maps through `display_offset`
            // (or reads nothing past the ring), so a reader built on it reads other
            // flags here.
            let viewport: Vec<bool> = (0..rows)
                .map(|row| t.grid().row(row).is_some_and(Row::is_wrapped))
                .collect();
            assert_ne!(viewport, live, "scrolled back {offset}");
            t.scroll_display(-offset);
        }
    }

    /// A history line past the read bound (a crafted checkpoint can store one) is read
    /// to the bound for search, its cells marked truncated, and whole only when the
    /// reader asks for the whole row.
    #[test]
    fn a_multi_mib_history_row_reads_bounded() {
        let mut line = b"NEEDLE ".to_vec();
        line.resize(2 * MAX_SCROLLBACK_LINE_SCAN_BYTES, b'A');
        let mut scrollback = Scrollback::with_defaults();
        scrollback.push_line(Line::from_bytes(&line));
        let t = Terminal::with_grid(Grid::with_tiered_scrollback(4, 80, 1000, scrollback));
        let oldest = t.grid().oldest_absolute_row();
        // NEGATIVE CONTROL: the stored line's own text runs past the bound.
        let stored = t.grid().get_history_line(0).expect("the line").to_string();
        assert_eq!(stored.len(), line.len());

        let mut rows = SearchRows::default();
        t.read_search_rows(oldest, 1, &mut rows);
        assert_eq!(rows.text[0].len(), MAX_SCROLLBACK_LINE_SCAN_BYTES);
        assert!(stored.starts_with(&rows.text[0]), "a prefix of the line");
        assert!(rows.cells[0].is_truncated(), "the cut is recorded");
        assert_eq!(
            t.abs_row_text(oldest).map(|text| text.len()),
            Some(MAX_SCROLLBACK_LINE_SCAN_BYTES)
        );
        assert_eq!(
            t.abs_row_text_within(oldest, usize::MAX),
            AbsRowText::Text(stored),
            "read whole when asked"
        );
        // A row at the bound is read whole and not marked.
        rows = SearchRows::default();
        t.read_search_rows(oldest + 1, 1, &mut rows);
        assert!(
            !rows.cells[0].is_truncated(),
            "the screen row is read whole"
        );
    }
}
