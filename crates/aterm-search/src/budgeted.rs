// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-buffer search (P1).
//!
//! A one-shot full-buffer search pays the whole index build + verification cost
//! in a single call — hundreds of milliseconds at deep scrollback, which blocks
//! the caller's event loop (the wasm render worker cannot answer input while
//! it runs). [`BudgetedSearch`] splits that work into row-sized units the
//! caller feeds incrementally: each `feed_row`
//! verifies ONE row with the batch index's matching machinery, so
//! a caller can stop after any number of rows, yield, and RESUME without
//! rebuilding anything. The row text and its coordinate map are discarded
//! after verification; only the bounded matches and scan progress survive.
//!
//! ## Results-equality contract
//!
//! Driving a `BudgetedSearch` to completion yields the SAME [`SearchResults`]
//! as building a [`SearchIndex`] over the same rows and calling
//! [`SearchIndex::search_results_opts`] — regardless of how the rows were
//! sliced across calls. This holds because every per-row verifier IS the batch
//! path's own machinery run over one row (the forward match iterator's scan
//! for case-sensitive literals, [`CaseInsensitiveMatcher`] for case-insensitive
//! literals, the same compiled/capped regex for regex mode), rows are fed in
//! the same ascending absolute order the batch build indexes them, and the
//! [`MAX_SEARCH_MATCHES`] cap is applied to exactly the final retained suffix.
//! Rows that deterministic index eviction would discard are consumed without
//! verification, preventing a capped evicted prefix from starving newer retained
//! matches. No index is retained: the same eviction schedule determines the
//! searchable suffix up front. Pinned by the slicing-oracle tests below,
//! including the combined cap-plus-eviction regime.
//!
//! ## Staleness
//!
//! `BudgetedSearch` knows nothing about content generations — it searches the
//! rows it is fed. The owner (aterm-core's `Terminal::search_budgeted`) keys
//! each instance to a content generation and DISCARDS it when the underlying
//! buffer changes, so a resumed cursor can never surface stale coordinates.

use std::borrow::Cow;

use crate::grapheme::ColumnMap;
use crate::index::{
    CaseInsensitiveMatcher, MAX_SEARCH_MATCHES, SearchIndex, SearchOptionsError,
    final_evicted_prefix,
};
use crate::iterators::next_literal_match;
use crate::types::{SearchMatch, SearchResults};

/// Per-row verifier, fixed at construction from `(case_sensitive, is_regex)`.
///
/// Each variant reuses the corresponding batch-search machinery so budgeted
/// results are equal to one-shot results by construction (module docs).
enum RowMatcher {
    /// Case-sensitive literal: verified with the batch forward iterator's own
    /// scan ([`next_literal_match`]) run over the supplied row.
    Literal,
    /// Case-insensitive literal: the batch path's per-line matcher.
    CaseInsensitive(CaseInsensitiveMatcher),
    /// Regex (either case mode): compiled ONCE with the batch path's pattern
    /// length / NFA / DFA caps, reused across every resumed slice.
    Regex(aterm_regex::Regex),
}

/// A budgeted, resumable search over a fixed window of absolute rows.
///
/// Construct with the query + options and the window `[base_row, base_row +
/// total_rows)`, then feed each row's text in ascending order via
/// `feed_row` — as few or as many per call as the caller's
/// budget allows. [`is_complete`](Self::is_complete) reports whether every row
/// has been consumed; [`results`](Self::results) returns the accumulated
/// [`SearchResults`] (partial until complete). See the module docs for the
/// results-equality and staleness contracts.
pub struct BudgetedSearch {
    /// The batch index's cache cap, preserved for its first-eviction warning.
    /// This engine retains no row cache of its own.
    max_cached_lines: usize,
    /// The query string (verbatim; folding/compilation lives in `matcher`).
    query: String,
    /// Per-row verifier fixed at construction.
    matcher: RowMatcher,
    /// Accumulated matches in ascending (line, col) order, capped at
    /// [`MAX_SEARCH_MATCHES`].
    matches: Vec<SearchMatch>,
    /// Absolute row number of the first row in the window.
    base_row: usize,
    /// Total rows in the window.
    total_rows: usize,
    /// Rows consumed so far; the next `feed_row` is absolute
    /// `base_row + rows_fed`.
    rows_fed: usize,
    /// Window-relative first row that the completed index will retain. Rows
    /// before this are consumed but deliberately not verified; see the
    /// equality contract.
    verify_from: usize,
    /// Final one-shot-compatible watermark, known from the deterministic
    /// eviction schedule before feeding begins. Zero means no eviction.
    final_lowest_retained_line: usize,
}

impl BudgetedSearch {
    /// Start a budgeted search for `query` over `total_rows` rows whose first
    /// row is absolute row `base_row`.
    ///
    /// Regex patterns are validated and compiled here (with the batch path's
    /// size caps), so an invalid pattern fails before any row scanning.
    pub fn new(
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        base_row: usize,
        total_rows: usize,
    ) -> Result<Self, SearchOptionsError> {
        Self::with_max_cached_lines(
            query,
            case_sensitive,
            is_regex,
            base_row,
            total_rows,
            crate::index::DEFAULT_MAX_CACHED_LINES,
        )
    }

    /// [`new`](Self::new) with an explicit index cache cap (eviction bound).
    pub fn with_max_cached_lines(
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        base_row: usize,
        total_rows: usize,
        max_cached_lines: usize,
    ) -> Result<Self, SearchOptionsError> {
        let matcher = if is_regex {
            RowMatcher::Regex(SearchIndex::compile_regex(query, case_sensitive)?)
        } else if case_sensitive {
            RowMatcher::Literal
        } else {
            RowMatcher::CaseInsensitive(CaseInsensitiveMatcher::new(query))
        };
        let max_cached_lines = max_cached_lines.max(1);
        let verify_from = final_evicted_prefix(total_rows, max_cached_lines);
        let final_lowest_retained_line = if verify_from > 0 {
            base_row.saturating_add(verify_from)
        } else {
            0
        };
        Ok(Self {
            max_cached_lines,
            query: query.to_string(),
            matcher,
            matches: Vec::new(),
            base_row,
            total_rows,
            rows_fed: 0,
            verify_from,
            final_lowest_retained_line,
        })
    }

    /// Consume and verify ONE row. Rows must be fed in ascending window order;
    /// this row is absolute `base_row + rows_fed()`. Feeding past the window
    /// is a no-op (the caller's completion check races nothing, so tolerate it
    /// rather than panic).
    #[cfg(test)]
    pub fn feed_row(&mut self, text: &str) {
        self.feed_row_cow(Cow::Borrowed(text));
    }

    /// Consume and verify one owned row without copying or retaining its text.
    ///
    /// This has the same ordering and completion contract as
    /// `feed_row`; `text` is released before returning.
    pub fn feed_row_owned(&mut self, text: String) {
        self.feed_row_cow(Cow::Owned(text));
    }

    fn feed_row_cow(&mut self, text: Cow<'_, str>) {
        if self.rows_fed >= self.total_rows {
            return;
        }
        let row_offset = self.rows_fed;
        let abs_row = self.base_row.saturating_add(row_offset);
        self.rows_fed += 1;
        // Preserve the batch index's once-per-search truncation diagnostic at
        // its first eviction boundary, without building its unused row cache.
        if self.verify_from > 0 && row_offset == self.max_cached_lines {
            let lowest = self
                .base_row
                .saturating_add(final_evicted_prefix(self.rows_fed, self.max_cached_lines));
            aterm_log::warn!(
                "budgeted search exceeded {} cached-line comparison limit; \
                 search results may be incomplete below line {}",
                self.max_cached_lines,
                lowest,
            );
        }
        if row_offset < self.verify_from {
            return;
        }
        // Once capped, further verification cannot change the result set (the
        // batch path stops scanning at the cap too). Because verification only
        // begins at the final retained suffix, later eviction can never remove
        // capped matches and expose an unverified hole.
        if self.matches.len() < MAX_SEARCH_MATCHES {
            self.verify_row(abs_row, &text);
        }
    }

    /// Rows consumed so far (also the window-relative index of the next
    /// expected row).
    #[must_use]
    pub fn rows_fed(&self) -> usize {
        self.rows_fed
    }

    /// Total rows in the search window.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    /// Whether every row in the window has been fed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.rows_fed >= self.total_rows
    }

    /// The accumulated results (partial until [`is_complete`](Self::is_complete)).
    ///
    /// Mirrors [`SearchIndex::search_results_opts`]'s reporting: `incomplete`
    /// is set when eviction dropped indexed rows OR the match cap was reached,
    /// and matches below the eviction watermark are filtered out so a
    /// completed budgeted search equals a one-shot search over the same rows.
    /// This is a full snapshot; resumable frontends should use
    /// [`results_range`](Self::results_range) to bound per-turn copying.
    #[must_use]
    pub fn results(&self) -> SearchResults {
        let capped = self.matches.len() >= MAX_SEARCH_MATCHES;
        let evicted = self.verify_from > 0;
        let lowest = self.final_lowest_retained_line;
        let matches = if evicted {
            // Keep the one-shot watermark boundary explicit. Verification
            // already skips the prefix its completed index would have evicted.
            self.matches
                .iter()
                .filter(|m| m.line >= lowest)
                .cloned()
                .collect()
        } else {
            self.matches.clone()
        };
        SearchResults::new(matches, evicted || capped, lowest)
    }

    /// Number of matches accumulated so far.
    ///
    /// This lets a resumable owner deliver each match exactly once without
    /// cloning the whole growing prefix on every event-loop turn.
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.matches.len()
    }

    /// Clone at most `limit` accumulated matches beginning at `start`.
    ///
    /// The returned metadata describes the whole search state, while
    /// `matches` is only the requested stable slice. Verification starts at
    /// the completed index's final retained suffix, so a slice already handed
    /// to a caller can never be invalidated by a later eviction pass.
    #[must_use]
    pub fn results_range(&self, start: usize, limit: usize) -> SearchResults {
        let start = start.min(self.matches.len());
        let end = start.saturating_add(limit).min(self.matches.len());
        let capped = self.matches.len() >= MAX_SEARCH_MATCHES;
        let evicted = self.verify_from > 0;
        let lowest = self.final_lowest_retained_line;
        debug_assert!(
            self.matches
                .first()
                .is_none_or(|first| first.line >= lowest)
        );
        SearchResults::new(self.matches[start..end].to_vec(), evicted || capped, lowest)
    }

    /// Verify matches on the supplied row `abs_row`, appending to
    /// `self.matches` (ascending order preserved; capped).
    fn verify_row(&mut self, abs_row: usize, text: &str) {
        if self.query.is_empty() && !matches!(&self.matcher, RowMatcher::Regex(_)) {
            return;
        }
        let col_map = ColumnMap::new(text);
        match &mut self.matcher {
            RowMatcher::Literal => {
                // Use the batch forward iterator's own per-row verifier.
                // Rebuilding postings for a row consumed exactly once would
                // add work without improving this scan. All three arms use
                // the same temporary coordinate map and retain only matches.
                let searcher = crate::bytesearch::Searcher::new(self.query.as_bytes());
                let mut next_byte = 0;
                while let Some((found, resume)) =
                    next_literal_match(abs_row, text, &searcher, &col_map, next_byte)
                {
                    next_byte = resume;
                    self.matches.push(found);
                    if self.matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
            }
            RowMatcher::CaseInsensitive(matcher) => {
                let matches = &mut self.matches;
                matcher.visit_matches(abs_row, text, &col_map, |found| {
                    matches.push(found);
                    matches.len() < MAX_SEARCH_MATCHES
                });
            }
            RowMatcher::Regex(re) => {
                for cap in re.find_iter(text) {
                    // Skip zero-length matches and byte spans that resolve to
                    // zero display columns — the batch forward path's rule.
                    if cap.start() == cap.end() {
                        continue;
                    }
                    let start_col = col_map.byte_to_column(cap.start());
                    let end_col = col_map.byte_to_column(cap.end());
                    if start_col == end_col {
                        continue;
                    }
                    self.matches
                        .push(SearchMatch::new(abs_row, start_col, end_col));
                    if self.matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-shot oracle: batch-build the index and run the batch search.
    fn one_shot(
        rows: &[&str],
        base: usize,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> SearchResults {
        let mut index = SearchIndex::new();
        for (i, row) in rows.iter().enumerate() {
            index.index_line(base + i, row);
        }
        index
            .search_results_opts(query, case_sensitive, is_regex)
            .expect("oracle search must succeed")
    }

    /// Drive a budgeted search to completion in `slice`-row steps.
    fn budgeted(
        rows: &[&str],
        base: usize,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        slice: usize,
    ) -> SearchResults {
        let mut search = BudgetedSearch::new(query, case_sensitive, is_regex, base, rows.len())
            .expect("budgeted construction must succeed");
        while !search.is_complete() {
            for _ in 0..slice.max(1) {
                if search.is_complete() {
                    break;
                }
                search.feed_row(rows[search.rows_fed()]);
            }
        }
        search.results()
    }

    fn sample_rows() -> Vec<&'static str> {
        vec![
            "the quick brown fox",
            "NEEDLE in a haystack",
            "needle needle needle",
            "",
            "aaaaaa overlapping aaa runs",
            "wide \u{4f60}\u{597d} needle after CJK",
            "N\u{e9}edle with accents \u{c4}rger",
            "tail needleneedle",
            "unrelated filler line",
            "last NEEDLE",
        ]
    }

    #[test]
    fn owned_feed_matches_borrowed_and_batch_unicode_coordinates() {
        let rows = [
            "e\u{301} 日本語 👩🏽‍💻 NEEDLE needle",
            "\tΟΣ ος οσ\r\nneedle",
            "tail e\u{301} 日本語 👩🏽‍💻",
        ];
        for (query, case_sensitive, is_regex) in [
            ("needle", true, false),
            ("οσ", false, false),
            ("e\u{301}|日本語|👩🏽‍💻", true, true),
            ("NEEDLE", false, true),
            ("", true, false),
            ("", false, true),
        ] {
            let mut owned = BudgetedSearch::new(query, case_sensitive, is_regex, 7, rows.len())
                .expect("budgeted construction");
            let mut borrowed = BudgetedSearch::new(query, case_sensitive, is_regex, 7, rows.len())
                .expect("budgeted construction");
            for row in rows {
                // Excess capacity must not affect matching or be kept by the
                // engine. Actual reclamation is checked by index_release_memory.
                let mut text = String::with_capacity(4096);
                text.push_str(row);
                owned.feed_row_owned(text);
                borrowed.feed_row(row);
                assert_eq!(owned.results(), borrowed.results());
                assert_eq!(owned.rows_fed(), borrowed.rows_fed());
            }
            assert!(owned.is_complete());
            assert_eq!(
                owned.results(),
                one_shot(&rows, 7, query, case_sensitive, is_regex),
                "query={query:?} cs={case_sensitive} rx={is_regex}"
            );
        }
    }

    /// The closed-form doomed-prefix calculation must stay identical to the
    /// real index's hysteretic 3/4 eviction schedule for every small boundary.
    #[test]
    fn final_evicted_prefix_matches_real_index_schedule() {
        for cap in 1..=20 {
            for total in 0..=100 {
                let mut index = SearchIndex::with_max_cached_lines(cap);
                for row in 0..total {
                    index.index_line(row, "row");
                }
                assert_eq!(
                    final_evicted_prefix(total, cap),
                    index.lowest_retained_line(),
                    "cap={cap} total={total}"
                );
            }
        }
    }

    /// Resume-equality oracle: any slicing of the row feed produces results
    /// identical to the one-shot batch search — every filter mode, including
    /// short (< trigram) and Unicode queries, at a non-zero base row.
    #[test]
    fn any_slicing_equals_one_shot_across_all_modes() {
        let rows = sample_rows();
        let cases: &[(&str, bool, bool)] = &[
            ("needle", true, false),  // case-sensitive literal
            ("needle", false, false), // case-insensitive literal
            ("NEEDLE", false, false), // case-insensitive, uppercase query
            ("aa", true, false),      // short query (no trigram accel)
            ("aa", false, false),
            ("\u{e9}", false, false),      // Unicode short query
            ("\u{c4}rger", false, false),  // Unicode folding
            ("ne+dle", true, true),        // regex, case-sensitive
            ("ne+dle", false, true),       // regex, case-insensitive
            ("n..dle", true, true),        // regex with wildcards
            ("x{0,3}needle", false, true), // regex with optional prefix
        ];
        for &(query, case_sensitive, is_regex) in cases {
            let oracle = one_shot(&rows, 37, query, case_sensitive, is_regex);
            for slice in [1, 2, 3, 7, rows.len()] {
                let got = budgeted(&rows, 37, query, case_sensitive, is_regex, slice);
                assert_eq!(
                    got, oracle,
                    "slice={slice} query={query:?} cs={case_sensitive} rx={is_regex}"
                );
            }
        }
    }

    /// The match cap applies identically to budgeted and one-shot searches,
    /// and both report the capped set as incomplete.
    #[test]
    fn match_cap_matches_one_shot_and_reports_incomplete() {
        // 80 matches per row x 1300 rows = 104_000 > MAX_SEARCH_MATCHES.
        let row = "a".repeat(80);
        let rows: Vec<&str> = (0..1_300).map(|_| row.as_str()).collect();
        let oracle = one_shot(&rows, 0, "a", true, false);
        assert!(oracle.incomplete, "cap must be reported as incomplete");
        assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
        let got = budgeted(&rows, 0, "a", true, false, 512);
        assert_eq!(got, oracle);
    }

    /// Eviction mid-build: the completed budgeted search equals the one-shot
    /// search over the same capped index (evicted rows filtered, incomplete
    /// reported, watermark equal).
    #[test]
    fn eviction_matches_one_shot_capped_index() {
        let rows: Vec<String> = (0..100).map(|i| format!("needle row {i}")).collect();
        let row_refs: Vec<&str> = rows.iter().map(String::as_str).collect();

        let mut oracle_index = SearchIndex::with_max_cached_lines(10);
        for (i, row) in row_refs.iter().enumerate() {
            oracle_index.index_line(i, row);
        }
        let oracle = oracle_index
            .search_results_opts("needle", true, false)
            .expect("oracle search");
        assert!(oracle.incomplete, "eviction must be reported");

        for slice in [1, 7, 100] {
            let mut search =
                BudgetedSearch::with_max_cached_lines("needle", true, false, 0, row_refs.len(), 10)
                    .expect("budgeted construction");
            while !search.is_complete() {
                for _ in 0..slice {
                    if search.is_complete() {
                        break;
                    }
                    search.feed_row(row_refs[search.rows_fed()]);
                }
            }
            assert_eq!(search.results(), oracle, "slice={slice}");
        }
    }

    /// Regression: reaching the match cap in rows that are later evicted must
    /// not starve matches in the final retained suffix. This is the interaction
    /// the separate cap-only and eviction-only tests cannot exercise.
    #[test]
    fn combined_match_cap_and_eviction_equals_one_shot() {
        const CACHE_LINES: usize = 10;
        const ROWS: usize = 20;
        // More than MAX_SEARCH_MATCHES matches occur before the final retained
        // suffix, while every retained row also contains matches.
        let dense = "a".repeat(MAX_SEARCH_MATCHES / 2 + 1);
        let rows: Vec<&str> = (0..ROWS).map(|_| dense.as_str()).collect();

        let mut oracle_index = SearchIndex::with_max_cached_lines(CACHE_LINES);
        for (i, row) in rows.iter().enumerate() {
            oracle_index.index_line(i, row);
        }
        let oracle = oracle_index
            .search_results_opts("a", true, false)
            .expect("oracle search");
        assert!(oracle.incomplete, "cap plus eviction must be reported");
        assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
        assert!(
            oracle
                .matches
                .iter()
                .all(|m| m.line >= oracle.lowest_retained_line),
            "oracle only returns the retained suffix"
        );

        for slice in [1, 3, ROWS] {
            let mut search =
                BudgetedSearch::with_max_cached_lines("a", true, false, 0, rows.len(), CACHE_LINES)
                    .expect("budgeted construction");
            while !search.is_complete() {
                for _ in 0..slice {
                    if search.is_complete() {
                        break;
                    }
                    search.feed_row(rows[search.rows_fed()]);
                }
            }
            assert_eq!(search.results(), oracle, "slice={slice}");
        }
    }

    /// An invalid regex fails at construction, before any row scanning.
    #[test]
    fn invalid_regex_fails_at_construction() {
        let err = BudgetedSearch::new("f(oo", false, true, 0, 10);
        assert!(matches!(err, Err(SearchOptionsError::InvalidRegex(_))));
    }

    /// Partial results are a prefix of the final results (progressive display
    /// never shows a match that later disappears absent invalidation), and
    /// feeding past the window is a tolerated no-op.
    #[test]
    fn partial_results_are_a_prefix_and_overfeed_is_a_noop() {
        let rows = sample_rows();
        let mut search =
            BudgetedSearch::new("needle", false, false, 0, rows.len()).expect("construction");
        let mut previous = 0usize;
        let mut seen: Vec<SearchMatch> = Vec::new();
        for row in &rows {
            search.feed_row(row);
            let partial = search.results();
            assert!(partial.matches.len() >= previous, "match count is monotone");
            assert_eq!(
                &partial.matches[..seen.len()],
                seen.as_slice(),
                "earlier matches are stable across slices"
            );
            previous = partial.matches.len();
            seen = partial.matches;
        }
        assert!(search.is_complete());
        let done = search.results();
        search.feed_row("needle beyond the window");
        assert_eq!(search.results(), done, "overfeed must not change results");
        assert_eq!(done, one_shot(&rows, 0, "needle", false, false));
    }

    /// Stable slices stay valid even when the batch index would cross an
    /// eviction boundary: doomed-prefix rows are never emitted, and bounded
    /// range reads do not clone the full accumulated vector.
    #[test]
    fn result_ranges_are_bounded_and_stable_across_eviction() {
        let rows: Vec<String> = (0..20).map(|i| format!("needle row {i}")).collect();
        let mut search =
            BudgetedSearch::with_max_cached_lines("needle", true, false, 0, rows.len(), 10)
                .expect("construction");

        for row in rows.iter().take(14) {
            search.feed_row(row);
        }
        let first = search.results_range(0, 2);
        assert!(first.matches.len() <= 2);
        assert!(first.matches.iter().all(|m| m.line >= 12));
        assert!(first.incomplete);
        assert_eq!(first.lowest_retained_line, 12);

        for row in rows.iter().skip(14) {
            search.feed_row(row);
        }
        let final_results = search.results();
        assert_eq!(
            &final_results.matches[..first.matches.len()],
            first.matches.as_slice(),
            "an emitted delta must remain a prefix after later eviction"
        );
        let second = search.results_range(first.matches.len(), 2);
        assert!(second.matches.len() <= 2);
        assert_eq!(second.lowest_retained_line, first.lowest_retained_line);
        assert_eq!(
            [first.matches, second.matches].concat(),
            final_results.matches[..4]
        );
    }
}
