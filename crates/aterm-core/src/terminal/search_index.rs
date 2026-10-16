// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cached full-content search index.
//!
//! The index over all retained scrollback plus visible rows depends only on which grid is
//! active and its content, so one index serves every query until the content changes.
//! [`Terminal::indexed_search`] caches it keyed by `(alternate_screen, content_seq())` plus
//! the renumber epoch and the ambiguous-width policy. The key cannot go stale: line text,
//! membership and `oldest_absolute_row()` change only by writes, scrolls, erases, reflows
//! and resizes, all of which bump the active grid's `content_gen`. A pure viewport scroll
//! reuses the cache.
//!
//! A miss refreshes in O(churn) rather than rebuilding in O(total):
//!
//! - Append: rows at or above the previous `indexed_end` are new; feed them.
//! - Replace: rows at or above the previous visible base may have been edited in place;
//!   re-feed them (unchanged rows are cheap).
//! - EvictBelow: `drop_history_below` removes evicted rows with fresh-build semantics.
//! - Reflow, alt-screen switch, protected-footer splice (`absolute_row_revision`) or Kitty
//!   unscroll (`history_renumber_epoch`): full rebuild.
//!
//! The refresh runs only when it provably equals the full rebuild (guards in
//! `try_refresh_search_index`); the tests compare it with `legacy_results`, a from-scratch
//! oracle.

use aterm_types::text_shaping::AmbiguousWidth;

use super::Terminal;
use super::search_rows::history_row_for_search;
use crate::search::{DEFAULT_MAX_CACHED_LINES, TerminalSearch};

/// A cached search index and the key it was built for, stamped at (re)build time.
pub(crate) struct CachedSearchIndex {
    /// The alternate screen was active: the grids keep independent `content_gen` counters,
    /// so a swap landing on the same value must still invalidate.
    alt_screen: bool,
    /// The active grid's `content_seq()`, bumped by every content mutation.
    content_gen: u64,
    /// Protected-footer splice revision: a splice renumbers rows piecewise (full rebuild).
    absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()`: a Kitty CSI +T unscroll shifts older rows' keys
    /// without moving the other stamps. Part of the hit key.
    history_renumber_epoch: u64,
    /// Grid width. A width change rewraps history (full rebuild); height-only changes move
    /// rows between history and visible without moving keys.
    cols: u16,
    /// The `ambiguous_width` policy the index measures under; a live switch moves no
    /// `content_gen`, so it is part of the hit key.
    ambiguous_width: AmbiguousWidth,
    /// Absolute row of the oldest retained line; a decrease means renumbering.
    hist_base: usize,
    /// Absolute row of the top visible row. Rows at or above it were fed as visible text
    /// and may be edited in place; rows below are immutable history.
    visible_base: usize,
    /// Exclusive end of the indexed absolute-row range; a shrink forces the full rebuild.
    indexed_end: usize,
    /// The index over scrollback and visible rows, reused verbatim while the key matches.
    index: TerminalSearch,
}

impl Terminal {
    /// A full-content search index, reused while the active grid's content is unchanged. It
    /// covers every retained line keyed by absolute row (history at `oldest + i`, visible
    /// rows at `oldest + scrollback + r`), so each
    /// [`SearchMatch::line`](crate::search::SearchMatch) is an absolute row. Query with
    /// `indexed_search(...).search_results_opts(pat, case, regex)`. A miss refreshes or
    /// rebuilds; the result is immutable so callers cannot alter cached coordinates.
    pub fn indexed_search(&mut self) -> &TerminalSearch {
        let key_alt = self.modes.alternate_screen;
        let key_gen = self.content_seq();
        // A Kitty unscroll shifts older rows' keys without necessarily moving
        // `content_seq`, so the renumber epoch is part of the hit key.
        let key_epoch = self.grid.history_renumber_epoch();
        let key_ambiguous = self.ambiguous_width();

        let hit = match &self.search_index {
            Some(cached) => {
                cached.alt_screen == key_alt
                    && cached.content_gen == key_gen
                    && cached.history_renumber_epoch == key_epoch
                    && cached.ambiguous_width == key_ambiguous
            }
            None => false,
        };

        if !hit {
            // Retained-window geometry for the guards, the refresh and the stamps; mirrors
            // `build_search_index`.
            let oldest = self.grid.oldest_absolute_row();
            let scrollback = self.grid.scrollback_lines();
            let rows = self.rows();
            let cols = self.cols();
            let revision = self.absolute_row_revision;
            let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
            let visible_base = hist_base.saturating_add(scrollback);
            let indexed_end = visible_base.saturating_add(usize::from(rows));

            let index = match self.try_refresh_search_index(
                key_alt,
                revision,
                key_epoch,
                cols,
                hist_base,
                visible_base,
                indexed_end,
            ) {
                Some(index) => {
                    self.search_index_refreshes = self.search_index_refreshes.wrapping_add(1);
                    index
                }
                // The full build is the fallback and the definition the refresh must equal.
                None => self.build_search_index(),
            };
            self.search_index = Some(CachedSearchIndex {
                alt_screen: key_alt,
                content_gen: key_gen,
                absolute_row_revision: revision,
                history_renumber_epoch: key_epoch,
                cols,
                ambiguous_width: key_ambiguous,
                hist_base,
                visible_base,
                indexed_end,
                index,
            });
            self.search_index_rebuilds = self.search_index_rebuilds.wrapping_add(1);
        }

        &self
            .search_index
            .as_ref()
            .expect("search_index populated above")
            .index
    }

    /// Serve a cache miss by refreshing the cached index; `None` unless the refresh provably
    /// equals `build_search_index` over the current window. Given the guards:
    ///
    /// - Rows below the previous visible base are immutable history: splices, rewraps and
    ///   renumberings are fenced to the rebuild, and retention only drops the oldest rows.
    ///   The refresh re-feeds from `min(previous, current visible base)`, so a reclassified
    ///   row is read as the build reads it (`written_line_cells` visible,
    ///   `history_row_for_search` history).
    /// - `drop_history_below` removes the evicted prefix with fresh-build semantics.
    /// - No cap eviction fires on either path: the guards require an eviction-free cached
    ///   index and a window under `DEFAULT_MAX_CACHED_LINES` (`TerminalSearch::new`'s cap).
    ///
    /// Cost: O(appended rows + visible rows). The bloom filter keeps stale bits from
    /// replaced rows (false-positive candidates only; saturation self-heals via
    /// `rebuild_bloom`).
    #[allow(
        clippy::too_many_arguments,
        reason = "geometry tuple computed once by the caller and shared with the cache stamps"
    )]
    fn try_refresh_search_index(
        &mut self,
        key_alt: bool,
        revision: u64,
        renumber_epoch: u64,
        cols: u16,
        hist_base: usize,
        visible_base: usize,
        indexed_end: usize,
    ) -> Option<TerminalSearch> {
        {
            let cached = self.search_index.as_ref()?;
            let reusable = cached.alt_screen == key_alt
                && cached.absolute_row_revision == revision
                && cached.history_renumber_epoch == renumber_epoch
                && cached.cols == cols
                // Every row is re-measured under a new ambiguous-width policy.
                && cached.ambiguous_width == self.ambiguous_width()
                // Retention only advances; a retreat means renumbering (a re-attached
                // offloaded reflow can grow scrollback back).
                && cached.hist_base <= hist_base
                // The index cannot truncate above; a shrinking end falls back.
                && cached.indexed_end <= indexed_end
                // Both windows eviction-free, for the fresh-equality argument.
                && indexed_end.saturating_sub(hist_base) <= DEFAULT_MAX_CACHED_LINES
                && !cached.index.results_may_be_incomplete();
            if !reusable {
                return None;
            }
        }
        let cached = self.search_index.take()?;
        let mut index = cached.index;

        index.drop_history_below(hist_base);

        // Replace + Append: re-feed from the older visible base (unchanged rows are cheap).
        let refresh_start = cached.visible_base.min(visible_base).max(hist_base);
        if refresh_start < visible_base {
            let start_hist = refresh_start.saturating_sub(hist_base);
            let count = visible_base.saturating_sub(refresh_start);
            // The dense walk keeps absolute-row keys (`None` is empty text); the repeat
            // pads should it be shorter than `count`.
            let lines = self
                .grid
                .history_lines_from(start_hist)
                .chain(std::iter::repeat_with(|| None))
                .take(count);
            index.index_numbered_cells_owned(lines.enumerate().map(|(j, line)| {
                let (text, cells) =
                    history_row_for_search(&self.grid, start_hist.saturating_add(j), line);
                (refresh_start.saturating_add(j), text, cells)
            }));
        }
        let rows = self.rows();
        index.index_numbered_cells_owned((0..rows).map(|r| {
            let (text, cells) = self.written_line_cells(i32::from(r)).unwrap_or_default();
            (visible_base.saturating_add(usize::from(r)), text, cells)
        }));
        Some(index)
    }

    /// A fresh full-content index over the active grid. Its line set, order and absolute-row
    /// keys define search results.
    fn build_search_index(&self) -> TerminalSearch {
        let grid = &self.grid;
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = self.rows();

        let mut search = TerminalSearch::new().with_ambiguous_width(self.ambiguous_width());

        // History at absolute oldest + i via the dense walk (`Grid::history_lines_from`,
        // item `i` == `get_history_line(i)`, an unreadable line an empty placeholder so
        // later lines keep their rows), decoding each warm block / cold page once. Text is
        // bounded by MAX_SCROLLBACK_LINE_SCAN_BYTES: a crafted checkpoint can inject a
        // multi-MiB `Line`. Streamed, so the scrollback text is never resident twice. Each
        // row carries the cells its text was read from, so a match is reported where drawn.
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        let history = grid
            .history_lines_from(0)
            .chain(std::iter::repeat_with(|| None))
            .take(scrollback);
        search.index_numbered_cells_owned(history.enumerate().map(|(i, line)| {
            let (text, cells) = history_row_for_search(grid, i, line);
            (hist_base.saturating_add(i), text, cells)
        }));

        // Visible rows at oldest + scrollback + r, combining-aware, read as written with
        // trailing blanks kept as each history line stores its row: a row's matches must
        // not change when it scrolls off (`foo$`, a query ending in spaces).
        let vis_base = hist_base.saturating_add(scrollback);
        search.index_numbered_cells_owned((0..rows).map(|r| {
            let (text, cells) = self.written_line_cells(i32::from(r)).unwrap_or_default();
            (vis_base.saturating_add(usize::from(r)), text, cells)
        }));

        search
    }

    /// Search-index cache misses so far (rebuilds and refreshes), never a reuse.
    /// Introspection only.
    #[must_use]
    #[inline]
    pub fn search_index_rebuilds(&self) -> u64 {
        self.search_index_rebuilds
    }

    /// Incremental refreshes among those misses; `search_index_rebuilds() -
    /// search_index_refreshes()` is the number of full rebuilds. Introspection only.
    #[must_use]
    #[inline]
    pub fn search_index_refreshes(&self) -> u64 {
        self.search_index_refreshes
    }

    /// Release the search index's heap: the cached index and any budgeted search (in flight
    /// or retained for `search_summary`). The whole `CachedSearchIndex` goes: an in-place
    /// [`SearchIndex::release`](aterm_search::SearchIndex::release) would leave the key
    /// matching an emptied index, a false empty hit.
    pub fn release_search_index(&mut self) {
        self.search_index = None;
        self.budgeted_search = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::Terminal;

    /// The from-scratch oracle: the index built the uncached, per-line way.
    pub(crate) fn legacy_results(t: &Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        use crate::search::TerminalSearch;
        let grid = t.grid();
        let oldest = grid.oldest_absolute_row();
        let scrollback = grid.scrollback_lines();
        let rows = t.rows() as usize;
        let mut search = TerminalSearch::new().with_ambiguous_width(t.ambiguous_width());
        let hist_base = usize::try_from(oldest).unwrap_or(usize::MAX);
        let history: Vec<_> = (0..scrollback)
            .map(|i| {
                let (text, cells) = grid
                    .get_history_line(i)
                    .map(|l| {
                        let text = l.to_string();
                        let cells = super::super::history_line_text_cells(grid, i, &l, &text);
                        (text, cells)
                    })
                    .unwrap_or_default();
                (hist_base + i, text, cells)
            })
            .collect();
        search.index_numbered_cells_owned(history);
        let vis_base = hist_base.saturating_add(scrollback);
        let visible: Vec<_> = (0..rows)
            .map(|r| {
                let (text, cells) = t.written_line_cells(r as i32).unwrap_or_default();
                (vis_base + r, text, cells)
            })
            .collect();
        search.index_numbered_cells_owned(visible);
        let res = search
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// History through a tiered store with a corrupt warm block between a needle in cold
    /// and one in hot: a skipping reader would shift later lines onto neighbours' rows.
    pub(crate) fn corrupt_tiered_terminal() -> (Terminal, usize) {
        use aterm_grid::Grid;
        use aterm_scrollback::Scrollback;
        // Cold holds the needle at 2, hot the one at 38; the corrupt block sits at the
        // warm front between them and nothing evicts it.
        let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
        for i in 0..40 {
            match i {
                2 => sb.push_str("NEEDLE_before the corrupt block"),
                38 => sb.push_str("NEEDLE_after the corrupt block"),
                _ => sb.push_str(&format!("carried {i:03}")),
            }
        }
        assert!(
            sb.cold_line_count() > 2,
            "the first needle must sit in cold"
        );
        let corrupt = 5;
        sb.inject_corrupted_warm_block(corrupt);
        let mut t = Terminal::with_grid(Grid::with_tiered_scrollback(6, 40, 5, sb));
        for i in 0..20 {
            t.process(format!("live {i:03}\r\n").as_bytes());
        }
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

    /// Over a corrupt tiered segment the cached index equals the oracle, and the needle
    /// after the corrupt block lands on its own row.
    #[test]
    fn search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
        let (mut t, corrupt) = corrupt_tiered_terminal();
        let got = cached_results(&mut t, "NEEDLE_");
        assert_eq!(got, legacy_results(&t, "NEEDLE_"));
        assert_eq!(got.len(), 2, "both needles are found: {got:?}");
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).expect("row");
        // The corrupt block's placeholder rows sit between the needles.
        assert_eq!(got[0].0, oldest + 2);
        assert_eq!(got[1].0, oldest + 38 + corrupt);
    }

    /// Absolute rows only grow and survive live upgrades, so a long-lived tab passes row
    /// `u32::MAX`: find (full build, refresh, regex and literal) keeps answering there.
    #[test]
    fn find_keeps_working_once_absolute_rows_pass_u32_max() {
        for start in [(1u64 << 32) - 30, (1u64 << 32) + 10] {
            let mut t = Terminal::new(6, 40);
            t.grid_mut().continue_absolute_numbering(start);
            for i in 0..40 {
                t.process(format!("filler {i}\r\n").as_bytes());
            }
            t.process(b"hello world\r\n");
            let first = cached_results(&mut t, "hello");
            assert_eq!(first, legacy_results(&t, "hello"), "start={start}");
            assert_eq!(first.len(), 1, "start={start}: {first:?}");
            let row = first[0].0;
            assert!(row as u64 >= start, "start={start}: row {row} is absolute");
            let regex = t
                .indexed_search()
                .search_results_opts("hel+o", false, true)
                .expect("valid regex");
            assert_eq!(
                regex.matches.iter().map(|m| m.line).collect::<Vec<_>>(),
                vec![row],
                "start={start}"
            );

            let refreshes = t.search_index_refreshes();
            for i in 0..10 {
                t.process(format!("more {i}\r\n").as_bytes());
            }
            t.process(b"hello again\r\n");
            let second = cached_results(&mut t, "hello");
            assert!(
                t.search_index_refreshes() > refreshes,
                "the incremental refresh served it"
            );
            assert_eq!(second, legacy_results(&t, "hello"), "start={start}");
            assert_eq!(second.len(), 2, "start={start}: {second:?}");
            assert_eq!(
                second[0].0, row,
                "start={start}: the first hit kept its row"
            );
        }
    }

    pub(crate) fn cached_results(t: &mut Terminal, pat: &str) -> Vec<(usize, usize, usize)> {
        let res = t
            .indexed_search()
            .search_results_opts(pat, false, false)
            .expect("search ok");
        res.matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect()
    }

    /// A line's matches do not change when it scrolls into history: both are read as
    /// written, trailing blanks kept, so `foo$` and a query ending in printed spaces answer
    /// the same in either place, for the full build, the refresh and the budgeted search.
    #[test]
    fn a_rows_matches_do_not_change_when_it_scrolls_into_history() {
        let regex = |t: &mut Terminal, pat: &str| -> Vec<(usize, usize, usize)> {
            let res = t
                .indexed_search()
                .search_results_opts(pat, false, true)
                .expect("search ok");
            res.matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let budgeted = |t: &mut Terminal, pat: &str| -> Vec<(usize, usize, usize)> {
            let step = t
                .search_budgeted(pat, false, true, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            step.results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let mut t = Terminal::new(6, 20);
        t.process(b"foo   \r\nbar \x1b[41m  \x1b[0m\r\nbaz\r\nfoo");
        assert_eq!(
            t.grid().scrollback_lines(),
            0,
            "precondition: all on screen"
        );
        let queries = ["foo$", "bar$", "foo  ", "bar   "];
        let on_screen: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert_eq!(
            on_screen[2],
            vec![(0, 0, 5)],
            "the spaces the program printed are on the line: {on_screen:?}"
        );
        assert_eq!(
            on_screen[0],
            vec![(3, 0, 3)],
            "only the line that ends at `foo` ends at `foo`"
        );
        let budgeted_on_screen: Vec<_> = queries.iter().map(|q| budgeted(&mut t, q)).collect();
        assert_eq!(budgeted_on_screen, on_screen, "the budgeted search");
        assert_eq!(
            t.abs_row_text(0).as_deref(),
            Some("foo   "),
            "a match's snippet is the line it came from"
        );

        let refreshes = t.search_index_refreshes();
        t.process(b"\r\nqux");
        let refreshed: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert!(
            t.search_index_refreshes() > refreshes,
            "precondition: a refresh served it"
        );
        assert_eq!(refreshed, on_screen, "the refreshed index");

        t.process(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6");
        assert!(
            t.grid().scrollback_lines() >= 4,
            "precondition: every fixture line is history now"
        );
        let in_history: Vec<_> = queries.iter().map(|q| regex(&mut t, q)).collect();
        assert_eq!(in_history, on_screen);
        let budgeted_in_history: Vec<_> = queries.iter().map(|q| budgeted(&mut t, q)).collect();
        assert_eq!(budgeted_in_history, on_screen, "the budgeted search");
        assert_eq!(t.abs_row_text(0).as_deref(), Some("foo   "), "the snippet");
    }

    /// Where the grid paints `needle`'s first character on visible `row`.
    fn painted_col(t: &Terminal, row: usize, needle: char) -> Option<usize> {
        let cols = usize::from(t.cols());
        (0..cols).find(|&col| {
            t.cell_grapheme(row, col).as_deref() == Some(needle.encode_utf8(&mut [0; 4]))
        })
    }

    /// The grid folds a character into the cell before it only when that cell is the one
    /// before the cursor, so cells written out of order or joined by DCH stay apart where a
    /// walk of the text joins them. Every reader (cached build and refresh, oracle,
    /// budgeted, literal and regex) finds `XYZ` where the grid paints it.
    #[test]
    fn find_reports_matches_in_the_cells_the_grid_drew_them_in() {
        let cases: &[(&str, &str)] = &[
            // Control: written in order, the grid pairs the flag.
            ("in order", "\u{1f1e6}\u{1f1e7}XYZ"),
            (
                "flag halves, right one first",
                "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ",
            ),
            (
                "skin tone before its base",
                "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5HXYZ",
            ),
            (
                "a cell ending in ZWJ, the next drawn first",
                "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3HXYZ",
            ),
            (
                "flag halves brought together by DCH",
                "\u{1f1e6}q\u{1f1e7}XYZ\x1b[1;3H\x1b[P",
            ),
            (
                "skin tone brought to its base by DCH",
                "\u{1f44d}q\u{1f3fd}XYZ\x1b[1;3H\x1b[P",
            ),
            // VS15 narrows a wide glyph and empties its right half, which reads as a
            // blank: a walk of the text counts the glyph's two columns and the blank.
            ("a wide glyph narrowed by VS15", "\u{231a}\u{fe0e}XYZ"),
        ];
        let budgeted = |t: &mut Terminal, pat: &str, regex: bool| -> Vec<(usize, usize, usize)> {
            let step = t
                .search_budgeted(pat, false, regex, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            step.results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect()
        };
        let mut wrong = Vec::new();
        for (label, input) in cases {
            let mut t = Terminal::new(4, 20);
            t.process(input.as_bytes());
            let painted = painted_col(&t, 0, 'X').expect("X is painted");
            let expected = vec![(0, painted, 3)];
            let regex = t
                .indexed_search()
                .search_results_opts("X.Z", false, true)
                .expect("regex search")
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect::<Vec<_>>();
            let found = [
                ("cached", cached_results(&mut t, "xyz")),
                ("oracle", legacy_results(&t, "xyz")),
                ("budgeted", budgeted(&mut t, "XYZ", false)),
                ("regex", regex),
                ("budgeted regex", budgeted(&mut t, "X.Z", true)),
            ];
            for (reader, got) in found {
                if got != expected {
                    wrong.push(format!(
                        "{label} ({reader}): painted {expected:?}, found {got:?}"
                    ));
                }
            }
            let refreshes = t.search_index_refreshes();
            t.process(b"\x1b[3;1Hmore");
            let refreshed = cached_results(&mut t, "xyz");
            assert!(
                t.search_index_refreshes() > refreshes,
                "{label}: a refresh served it"
            );
            if refreshed != expected {
                wrong.push(format!(
                    "{label} (refresh): painted {expected:?}, found {refreshed:?}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "find names cells the match is not drawn in:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// A redraw that moves cells under unchanged text moves the refreshed matches with them.
    #[test]
    fn a_redraw_that_moves_cells_under_the_same_text_moves_the_matches() {
        let mut t = Terminal::new(4, 20);
        t.process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ".as_bytes());
        assert_eq!(cached_results(&mut t, "xyz"), vec![(0, 4, 3)]);
        t.process("\x1b[1;1H\x1b[2K\u{1f1e6}\u{1f1e7}XYZ".as_bytes());
        assert_eq!(
            t.written_line_text(0).as_deref(),
            Some("\u{1f1e6}\u{1f1e7}XYZ")
        );
        assert_eq!(
            painted_col(&t, 0, 'X'),
            Some(2),
            "precondition: the pair folded"
        );
        assert_eq!(cached_results(&mut t, "xyz"), vec![(0, 2, 3)]);
    }

    /// Printable ASCII after a cell ending in a ZWJ: the grid draws the `Q` of `❤️‍Qa` in a
    /// cell of its own, two columns right of the emoji, on either history tier, and find
    /// reports it there.
    #[test]
    fn ascii_after_a_zwj_is_found_where_either_history_tier_draws_it() {
        use aterm_grid::test_counters::take_ring_fast_materialize;
        for (tier, ring) in [("ring tier", None), ("past the ring", Some(1))] {
            let mut t = match ring {
                None => Terminal::new(3, 12),
                Some(ring) => Terminal::with_scrollback(
                    3,
                    12,
                    ring,
                    crate::scrollback::Scrollback::with_defaults(),
                ),
            };
            t.process("\u{2764}\u{fe0f}\u{200d}Qa\r\nmore\r\n\r\n\r\n\r\n".as_bytes());
            let oldest = t.grid().scrollback_lines() - 1;
            let _ = take_ring_fast_materialize();
            let _ = t.grid().materialize_scrollback_row_full(oldest, t.cols());
            assert_eq!(
                take_ring_fast_materialize(),
                usize::from(ring.is_none()),
                "precondition: the row is read from the {tier}"
            );
            t.scroll_to_top();
            let painted: Vec<_> = (0..4)
                .map(|col| t.display_cell_grapheme(0, col).expect("in range"))
                .collect();
            t.scroll_to_bottom();
            assert_eq!(
                painted,
                ["\u{2764}\u{fe0f}\u{200d}", "", "Q", "a"],
                "{tier}: the glass draws Q in a cell of its own"
            );
            assert_eq!(cached_results(&mut t, "q"), vec![(0, 2, 1)], "{tier}");
            let step = t
                .search_budgeted("Q", false, false, None, 100)
                .expect("budgeted search");
            assert!(step.complete, "one step covers the fixture");
            let budgeted: Vec<_> = step
                .results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect();
            assert_eq!(budgeted, vec![(0, 2, 1)], "{tier}: the budgeted search");
        }
    }

    /// The shapes of `find_reports_matches_in_the_cells_the_grid_drew_them_in`, scrolled into
    /// history: `XYZ` is found where either tier draws it. The ring tier draws a row from
    /// its own cells, which keep the shapes apart; a row past the ring from its stored line,
    /// whose walk joins them.
    #[test]
    fn find_reports_matches_where_either_history_tier_draws_them() {
        use aterm_grid::test_counters::take_ring_fast_materialize;
        let cases: &[(&str, &str)] = &[
            ("in order", "\u{1f1e6}\u{1f1e7}XYZ"),
            (
                "flag halves, right one first",
                "\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ",
            ),
            (
                "skin tone before its base",
                "\x1b[1;3H\u{1f3fd}\x1b[1;1H\u{1f44d}\x1b[1;5HXYZ",
            ),
            (
                "a cell ending in ZWJ, the next drawn first",
                "\x1b[1;2H\u{e9}\x1b[1;1Ha\u{200d}\x1b[1;3HXYZ",
            ),
            (
                "flag halves brought together by DCH",
                "\u{1f1e6}q\u{1f1e7}XYZ\x1b[1;3H\x1b[P",
            ),
            ("a wide glyph narrowed by VS15", "\u{231a}\u{fe0e}XYZ"),
        ];
        let mut wrong = Vec::new();
        for (tier, ring) in [("ring tier", None), ("past the ring", Some(1))] {
            for (label, input) in cases {
                let mut t = match ring {
                    None => Terminal::new(4, 20),
                    Some(ring) => Terminal::with_scrollback(
                        4,
                        20,
                        ring,
                        crate::scrollback::Scrollback::with_defaults(),
                    ),
                };
                t.process(input.as_bytes());
                t.process(b"\x1b[4;1H\r\n\r\n\r\n\r\n");
                let oldest = t.grid().scrollback_lines() - 1;
                let _ = take_ring_fast_materialize();
                let _ = t.grid().materialize_scrollback_row_full(oldest, t.cols());
                assert_eq!(
                    take_ring_fast_materialize(),
                    usize::from(ring.is_none()),
                    "{tier}, {label}: precondition: the row is drawn from the {tier}"
                );
                t.scroll_to_top();
                let painted = (0..usize::from(t.cols()))
                    .find(|&col| t.display_cell_grapheme(0, col).as_deref() == Some("X"))
                    .expect("X is painted");
                t.scroll_to_bottom();
                let expected = vec![(0, painted, 3)];
                let step = t
                    .search_budgeted("XYZ", false, false, None, 100)
                    .expect("budgeted search");
                assert!(step.complete, "one step covers the fixture");
                let budgeted: Vec<_> = step
                    .results
                    .matches
                    .iter()
                    .map(|m| (m.line, m.start_col, m.len()))
                    .collect();
                for (reader, got) in [
                    ("cached", cached_results(&mut t, "xyz")),
                    ("oracle", legacy_results(&t, "xyz")),
                    ("budgeted", budgeted),
                ] {
                    if got != expected {
                        wrong.push(format!(
                            "{tier}, {label} ({reader}): painted {expected:?}, found {got:?}"
                        ));
                    }
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "find names history cells the match is not drawn in:\n  {}",
            wrong.join("\n  ")
        );
    }

    /// A terminal whose `ambiguous_width` policy is `"wide"`.
    fn wide_terminal(rows: u16, cols: u16) -> Terminal {
        let mut t = Terminal::new(rows, cols);
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        t
    }

    /// With `ambiguous_width = "wide"` find reports the cells an East Asian Ambiguous
    /// character is drawn in (two), one-shot and budgeted.
    #[test]
    fn find_measures_ambiguous_characters_as_the_grid_draws_them() {
        let mut t = wide_terminal(4, 20);
        t.process("→→foo bar\r\n─── foo".as_bytes());
        assert!(
            t.grid().is_wide_continuation_at(0, 3) && t.grid().is_wide_continuation_at(1, 5),
            "precondition: the grid draws them wide"
        );
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 4, 3), (1, 7, 3)]);
        assert_eq!(legacy_results(&t, "foo"), cached_results(&mut t, "foo"));
        let budgeted = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(budgeted.complete);
        let budgeted: Vec<_> = budgeted
            .results
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect();
        assert_eq!(budgeted, vec![(0, 4, 3), (1, 7, 3)]);
    }

    /// A live switch to `"wide"` moves no content generation, so the next search must not
    /// serve the narrow index for a line written after it.
    #[test]
    fn a_live_switch_to_wide_ambiguous_width_is_not_served_from_the_narrow_index() {
        let mut t = Terminal::new(4, 20);
        t.process(b"plain foo\r\n");
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 6, 3)]);
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        t.process("→→foo".as_bytes());
        assert_eq!(cached_results(&mut t, "foo"), vec![(0, 6, 3), (1, 4, 3)]);
    }

    /// A live switch with no new output: the cached index, a resumed budgeted search and the
    /// summary each answer as a fresh build would. A row written before the switch keeps
    /// the cells it was drawn in (`apply_config` reflows nothing), and find reads them.
    #[test]
    fn a_live_switch_with_no_new_output_is_not_served_from_a_cache() {
        let mut t = Terminal::new(4, 20);
        t.process("→→foo\r\nx\r\nfoo".as_bytes());
        let narrow = cached_results(&mut t, "foo");
        let step = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(step.complete, "precondition: a completed budgeted scan");
        let partial = t
            .search_budgeted("foo", false, false, None, 1)
            .expect("budgeted search");
        let cursor = partial.cursor.expect("precondition: a scan in flight");

        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        let fresh = legacy_results(&t, "foo");
        assert_eq!(
            painted_col(&t, 0, 'f'),
            Some(2),
            "the grid reflowed nothing"
        );
        assert_eq!(
            fresh, narrow,
            "a row keeps the cells it was drawn in: {fresh:?}"
        );
        assert_eq!(fresh.first(), Some(&(0, 2, 3)));
        assert_eq!(cached_results(&mut t, "foo"), fresh, "the cached index");

        let resumed = t
            .search_budgeted("foo", false, false, Some(cursor), 100)
            .expect("budgeted search");
        assert!(
            resumed.reset,
            "the in-flight scan restarts under the new policy"
        );

        let mut t = Terminal::new(4, 20);
        t.process("→→foo\r\nx\r\nfoo".as_bytes());
        let step = t
            .search_budgeted("foo", false, false, None, 100)
            .expect("budgeted search");
        assert!(step.complete, "precondition: a completed budgeted scan");
        t.apply_config(&crate::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        let summary: Vec<_> = t
            .search_summary_results("foo", false, false)
            .expect("summary")
            .matches
            .iter()
            .map(|m| (m.line, m.start_col, m.len()))
            .collect();
        assert_eq!(summary, fresh, "the summary");
    }

    /// A repeat search reuses the cache (any pattern); a write invalidates it.
    #[test]
    fn repeat_search_reuses_cache_write_invalidates() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }

        let before = t.search_index_rebuilds();
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            before + 1,
            "first search must rebuild the index once"
        );
        assert_eq!(
            r1,
            legacy_results(&t, "NEEDLE_alpha"),
            "results must match the legacy index"
        );
        assert_eq!(r1.len(), 1, "the scrolled-off needle is found exactly once");

        let rebuilds_after_first = t.search_index_rebuilds();
        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_after_first,
            "the repeat search must REUSE the cache (no rebuild) — the O(1) win"
        );
        assert_eq!(
            r1, r2,
            "reused-cache results must be identical to the first search"
        );

        let r_filler = cached_results(&mut t, "filler");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_after_first,
            "a different pattern on unchanged content must NOT rebuild"
        );
        assert_eq!(r_filler, legacy_results(&t, "filler"));
        assert!(r_filler.len() >= 2, "many filler rows match");

        t.process(b"NEEDLE_beta later\r\n");
        let rebuilds_before_write_search = t.search_index_rebuilds();
        let r_beta = cached_results(&mut t, "NEEDLE_beta");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds_before_write_search + 1,
            "a search after a content write must rebuild the index"
        );
        assert_eq!(r_beta, legacy_results(&t, "NEEDLE_beta"));
        assert_eq!(r_beta.len(), 1, "the freshly written needle is found");

        assert_eq!(cached_results(&mut t, "NEEDLE_alpha"), r1);
    }

    /// A height-only resize keeps all history searchable at unshifted absolute rows.
    #[test]
    fn height_only_resize_keeps_history_searchable_with_stable_rows() {
        let mut t = Terminal::new(10, 40);
        for i in 0..30 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 30, "every written line matches pre-resize");

        t.resize(5, 40);
        let after = cached_results(&mut t, "needle-");
        assert_eq!(
            after.len(),
            30,
            "history stays searchable across a height-only resize"
        );
        assert_eq!(
            after,
            legacy_results(&t, "needle-"),
            "the post-resize cache equals a from-scratch rebuild (not stale)"
        );
        let hit = |t: &mut Terminal, pat: &str| cached_results(t, pat)[0].0;
        assert_eq!(hit(&mut t, "needle-0"), 0, "unmoved line keeps its row");
        assert_eq!(hit(&mut t, "needle-20"), 20, "unmoved line keeps its row");
        assert_eq!(t.grid().oldest_absolute_row(), 0, "nothing was evicted");
    }

    /// After `set_scrollback_line_limit` evicts lines, search returns no evicted rows, on
    /// the tiered store and on the ring-only grid (the wasm engines' shape).
    #[test]
    fn retention_shrink_invalidates_the_cached_index() {
        // A small ring, so the bulk of history lives in the store.
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut tiered = Terminal::with_scrollback(6, 40, 8, sb);
        let check = |t: &mut Terminal, label: &str| {
            for i in 0..200 {
                t.process(format!("needle-{i}\r\n").as_bytes());
            }
            let before = cached_results(t, "needle-");
            assert_eq!(before.len(), 200, "{label}: all lines retained pre-shrink");

            t.set_scrollback_line_limit(Some(50));

            let after = cached_results(t, "needle-");
            assert_eq!(
                after,
                legacy_results(t, "needle-"),
                "{label}: the post-shrink search equals a from-scratch rebuild (not stale)"
            );
            let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
            assert!(
                after.iter().all(|&(line, _, _)| line >= oldest),
                "{label}: no match may point at an evicted absolute row (< {oldest})"
            );
            assert!(
                after.len() < before.len(),
                "{label}: the shrink really evicted matches"
            );
        };
        check(&mut tiered, "tiered");

        let mut ring = Terminal::new(6, 40);
        check(&mut ring, "ring-only");
    }

    /// A memory-budget shrink evicts cold scrollback, so the search cache invalidates.
    #[test]
    fn memory_budget_shrink_invalidates_the_cached_index() {
        // Small hot/warm tiers spill history to the cold tier, which a tight budget evicts.
        let sb = aterm_scrollback::Scrollback::new(8, 16, 64_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..400 {
            t.process(format!("needle-{i}\r\n").as_bytes());
        }
        let before = cached_results(&mut t, "needle-");
        assert_eq!(before.len(), 400, "all lines retained + found pre-shrink");
        let lines_before = t.grid().scrollback_lines();

        // An EnforcementFailed (the hot tier holds active data) is ignored: the cold
        // eviction still happened.
        let _ = t.set_memory_budget(4096);
        assert!(
            t.grid().scrollback_lines() < lines_before,
            "a tight budget must really evict retained lines \
             ({} -> {})",
            lines_before,
            t.grid().scrollback_lines()
        );

        // No write since the prime: the cache still rebuilds.
        let after = cached_results(&mut t, "needle-");
        assert_eq!(
            after,
            legacy_results(&t, "needle-"),
            "the post-budget-shrink search equals a from-scratch rebuild (not stale)"
        );
        let oldest = usize::try_from(t.grid().oldest_absolute_row()).unwrap();
        assert!(
            after.iter().all(|&(line, _, _)| line >= oldest),
            "no match may point at an evicted absolute row (< {oldest})"
        );
        assert!(
            after.len() < before.len(),
            "the budget shrink really evicted matches ({} -> {})",
            before.len(),
            after.len()
        );
    }

    /// `release_search_index` makes the next search rebuild, identically (never a false
    /// empty hit).
    #[test]
    fn release_drops_cache_then_next_search_rebuilds_identically() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(r1.len(), 1);
        let rebuilds = t.search_index_rebuilds();

        t.release_search_index();
        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds + 1,
            "release must force the next search to rebuild (real reclaim)"
        );
        assert_eq!(
            r2, r1,
            "post-release results are byte-identical (not empty)"
        );
    }

    /// A pure viewport scroll moves no `content_seq`, so the cache is reused.
    #[test]
    fn viewport_scroll_reuses_cache() {
        let mut t = Terminal::new(6, 40);
        t.process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            t.process(format!("filler line {i}\r\n").as_bytes());
        }
        let r1 = cached_results(&mut t, "NEEDLE_alpha");
        let rebuilds = t.search_index_rebuilds();
        let gen_before = t.content_seq();

        t.grid_mut().scroll_display(5);
        assert_eq!(
            t.content_seq(),
            gen_before,
            "a pure viewport scroll must NOT bump content_seq"
        );

        let r2 = cached_results(&mut t, "NEEDLE_alpha");
        assert_eq!(
            t.search_index_rebuilds(),
            rebuilds,
            "a pure viewport scroll must NOT invalidate the search cache"
        );
        assert_eq!(r1, r2, "results identical across a viewport scroll");
    }

    /// `restore_checkpoint` replaces the grids, whose fresh stamps could collide with the
    /// cache key, so it releases the index and the next search rebuilds.
    #[test]
    fn restore_checkpoint_drops_the_cached_search_index() {
        let mut source = Terminal::new(6, 40);
        for i in 0..30 {
            source.process(format!("alpha {i} needle\r\n").as_bytes());
        }
        let saved = source.checkpoint();

        let mut target = Terminal::new(6, 40);
        for i in 0..30 {
            target.process(format!("beta {i} other\r\n").as_bytes());
        }
        let primed = cached_results(&mut target, "needle");
        assert!(
            primed.is_empty(),
            "precondition: no needle before the restore"
        );

        target.restore_checkpoint(&saved);
        let rebuilds_before = target.search_index_rebuilds();
        let after = cached_results(&mut target, "needle");
        assert_eq!(
            target.search_index_rebuilds(),
            rebuilds_before + 1,
            "the restored buffer must be indexed afresh, never served from the \
             pre-restore cache"
        );
        assert_eq!(
            after,
            legacy_results(&target, "needle"),
            "post-restore search must equal a from-scratch rebuild"
        );
        assert!(!after.is_empty(), "the restored content is searchable");
    }

    /// A long deterministic mix of appends, edits, erases, scrollback clears, resizes,
    /// alt-screen switches, retention shrinks and Kitty unscrolls, searching after every
    /// step: the cached path equals `legacy_results` each time, over both arms.
    #[test]
    fn scripted_mutation_mixture_keeps_the_refresh_equal_to_a_rebuild() {
        let sb = aterm_scrollback::Scrollback::new(64, 512, 8_000_000);
        let mut t = Terminal::with_scrollback(8, 40, 64, sb);
        t.set_scrollback_line_limit(Some(600));

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut roll = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut written = 0usize;
        let mut on_alt = false;
        let mut deepest = 0usize;
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let refreshes_at_start = t.search_index_refreshes();
        let fulls_at_start = full_rebuilds(&t);

        for step in 0..140u64 {
            let r = roll();
            match r % 12 {
                // Streaming output, most of the script, so history accumulates.
                0..=6 => {
                    for _ in 0..=(r >> 8) % 14 {
                        t.process(format!("needle {written} alpha row\r\n").as_bytes());
                        written += 1;
                    }
                }
                // In-place edit of visible rows (the only mutable rows).
                7 => {
                    t.process(b"\x1b[H");
                    t.process(format!("beta {step} needle overwrite").as_bytes());
                }
                8 => {
                    if r % 48 == 8 {
                        t.process(b"\x1b[3J");
                    } else {
                        t.process(b"\x1b[2J");
                    }
                }
                // Height-only reclassifies rows without moving keys; a width change
                // rewraps history (rebuild fence).
                9 => {
                    if r % 2 == 0 {
                        let cols = t.cols();
                        t.resize(4 + u16::try_from(r % 8).unwrap_or(0), cols);
                    } else {
                        let rows = t.rows();
                        t.resize(rows, 24 + u16::try_from(r % 24).unwrap_or(0));
                    }
                }
                // Alt-screen switch (rebuild fence, both directions).
                10 => {
                    if on_alt {
                        t.process(b"\x1b[?1049l");
                    } else {
                        t.process(b"\x1b[?1049h");
                    }
                    on_alt = !on_alt;
                }
                // Kitty CSI +T unscroll (seen only via the epoch), and sometimes a
                // retention shrink.
                _ => {
                    if !on_alt {
                        let n = 1 + usize::try_from(r % 3).unwrap_or(0);
                        let _ = t.grid_mut().unscroll_from_scrollback(n);
                    }
                    if r % 3 == 0 {
                        t.set_scrollback_line_limit(Some(
                            250 + usize::try_from(r % 350).unwrap_or(0),
                        ));
                    }
                }
            }
            deepest = deepest.max(t.grid().scrollback_lines());

            for query in ["needle", "alpha", "beta"] {
                assert_eq!(
                    cached_results(&mut t, query),
                    legacy_results(&t, query),
                    "step {step} (roll {r:#x}), query {query:?}: the cached path \
                     must equal a from-scratch rebuild"
                );
            }
        }

        assert!(
            deepest >= 150,
            "the script must build REAL history depth for the oracle to mean \
             anything (deepest scrollback was {deepest})"
        );
        assert!(
            t.search_index_refreshes() > refreshes_at_start,
            "the script must exercise the incremental refresh arm"
        );
        assert!(
            full_rebuilds(&t) > fulls_at_start,
            "the script must also exercise the full-rebuild fallback arm"
        );
    }

    /// After streaming output every miss is an incremental refresh equal to the oracle.
    #[test]
    fn streaming_output_refreshes_incrementally_and_equals_legacy() {
        let mut t = Terminal::new(6, 40);
        for i in 0..50 {
            t.process(format!("seed line {i} needle\r\n").as_bytes());
        }
        let r0 = cached_results(&mut t, "needle");
        assert_eq!(r0, legacy_results(&t, "needle"));
        let full_rebuilds = |t: &Terminal| t.search_index_rebuilds() - t.search_index_refreshes();
        let full_before = full_rebuilds(&t);

        for batch in 0..5 {
            for i in 0..8 {
                t.process(format!("batch {batch} line {i} needle\r\n").as_bytes());
            }
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "a streaming-output miss must be served by the churn refresh"
            );
            assert_eq!(
                got,
                legacy_results(&t, "needle"),
                "refreshed results must equal a from-scratch rebuild"
            );
        }
        assert_eq!(
            full_rebuilds(&t),
            full_before,
            "no full O(total) rebuild may hide inside the churn loop"
        );
    }

    /// Ring-cap eviction while streaming (a capped session's steady state) stays on the
    /// refresh path and matches the oracle's matches and completeness.
    #[test]
    fn ring_eviction_steady_state_refreshes_and_equals_legacy() {
        let mut t = crate::terminal::TerminalBuilder::new()
            .size(6, 40)
            .ring_buffer_size(30)
            .build();
        for i in 0..60 {
            t.process(format!("fill {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        // Enough single-row advances to force several amortized compactions, with Unicode
        // columns and overlapping case-folded postings.
        for batch in 0..160 {
            let oldest_before = t.grid().oldest_absolute_row();
            t.process(format!("more {batch} NEEDLE 日本語 needle\r\n").as_bytes());
            assert!(
                t.grid().oldest_absolute_row() > oldest_before,
                "precondition: the full ring must be evicting between searches"
            );
            let refreshes_before = t.search_index_refreshes();
            let got = cached_results(&mut t, "needle");
            assert_eq!(
                t.search_index_refreshes(),
                refreshes_before + 1,
                "ring-eviction churn must stay on the refresh path"
            );
            assert_eq!(got, legacy_results(&t, "needle"));
            for query in ["NEEDLE", "日本語", "more"] {
                assert_eq!(cached_results(&mut t, query), legacy_results(&t, query));
            }
            let res = t
                .indexed_search()
                .search_results_opts("needle", false, false)
                .expect("search ok");
            assert!(
                !res.incomplete,
                "grid-retention drops are fresh-build-complete, never sticky-incomplete"
            );
        }
    }

    /// A width change (history rewrapped) and a Kitty CSI +T unscroll (history renumbered)
    /// both fall back to the full rebuild and advance the renumber epoch.
    #[test]
    fn width_change_and_unscroll_rebuild_instead_of_refreshing() {
        let mut t = Terminal::new(6, 40);
        for i in 0..40 {
            t.process(format!("wrapline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let refreshes = t.search_index_refreshes();
        let epoch = t.grid().history_renumber_epoch();
        t.resize(6, 33);
        // The `cols` fence alone would pass the refresh assertion below; without the
        // width-reflow epoch bump a later width change restoring `cols` would refresh over
        // renumbered keys.
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "a width rewrap renumbers history and must advance the epoch"
        );
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a width change must NOT take the refresh arm (history rewrapped)"
        );
        assert_eq!(got, legacy_results(&t, "needle"));

        // Unscroll arm: a tiered store, since a ring-only grid routes unscroll to a plain
        // region scroll.
        let sb = aterm_scrollback::Scrollback::new(8, 64, 8_000_000);
        let mut t = Terminal::with_scrollback(6, 40, 8, sb);
        for i in 0..80 {
            t.process(format!("uline {i} needle\r\n").as_bytes());
        }
        let _ = cached_results(&mut t, "needle");
        let epoch = t.grid().history_renumber_epoch();
        let removed = t.grid_mut().unscroll_from_scrollback(3);
        assert!(
            removed > 0,
            "precondition: unscroll must remove history lines"
        );
        assert!(
            t.grid().history_renumber_epoch() > epoch,
            "unscroll must advance the renumber epoch (the rebuild fence)"
        );
        let refreshes = t.search_index_refreshes();
        let got = cached_results(&mut t, "needle");
        assert_eq!(
            t.search_index_refreshes(),
            refreshes,
            "a history renumbering must force a full rebuild"
        );
        assert_eq!(got, legacy_results(&t, "needle"));
    }
}
