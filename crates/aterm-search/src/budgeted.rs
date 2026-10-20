// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-buffer search.
//!
//! A one-shot search pays the whole index build and verification in one call,
//! blocking the caller's event loop at deep scrollback. [`BudgetedSearch`] verifies
//! one fed row at a time, so a caller can stop after any row, yield, and resume.
//! Row text is dropped after verification; only the bounded matches and progress
//! survive.
//!
//! Driving a `BudgetedSearch` to completion yields the same [`SearchResults`] as
//! [`SearchIndex::search_results_opts`] over the same rows, wherever the caller
//! stops and resumes (it feeds one row per call): each per-row verifier is the
//! batch path's own (the forward iterator's
//! scan, [`CaseInsensitiveMatcher`], the same capped regex and [`scan_line`]), rows
//! arrive in the batch build's order, and [`MAX_SEARCH_MATCHES`] applies to the
//! final retained suffix. Rows the deterministic eviction schedule would drop are
//! consumed unverified, so a capped evicted prefix cannot starve newer matches. A
//! row whose regex scan runs out of budget keeps the matches found so far and counts
//! in [`SearchResults::cut_short_lines`], as in the one-shot search.
//!
//! The engine knows nothing about content generations; its owner
//! (aterm-core's `Terminal::search_budgeted`) discards it when the content changes.

use std::borrow::Cow;

use aterm_types::text_shaping::AmbiguousWidth;

use crate::cells::LineCells;
use crate::grapheme::ColumnMap;
use crate::index::{
    CaseInsensitiveMatcher, MAX_SEARCH_MATCHES, SearchIndex, SearchOptionsError,
    final_evicted_prefix, scan_line,
};
use crate::iterators::next_literal_match;
use crate::types::{SearchMatch, SearchResults};

/// Per-row verifier, fixed by `(case_sensitive, is_regex)`; each reuses the batch
/// path's machinery.
enum RowMatcher {
    /// Case-sensitive literal: the batch forward iterator's scan
    /// ([`next_literal_match`]).
    Literal,
    /// Case-insensitive literal: the batch path's per-line matcher.
    CaseInsensitive(CaseInsensitiveMatcher),
    /// Regex (either case mode), compiled once with the batch path's caps.
    Regex(aterm_regex::Regex),
}

/// A budgeted, resumable search over the absolute rows
/// `[base_row, base_row + total_rows)`, fed one row at a time in ascending order.
/// See the module docs for the results-equality contract.
pub struct BudgetedSearch {
    /// The batch index's cache cap; no row cache is kept here.
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
    /// Rows consumed so far.
    rows_fed: usize,
    /// Window-relative first row the completed index retains; earlier rows are
    /// consumed unverified.
    verify_from: usize,
    /// The one-shot watermark, known up front from the eviction schedule; zero
    /// means no eviction.
    final_lowest_retained_line: usize,
    /// The `ambiguous_width` policy match columns are measured under.
    ambiguous_width: AmbiguousWidth,
    /// Verified rows whose regex scan ran out of its budget part-way
    /// ([`SearchResults::cut_short_lines`]).
    cut_short_rows: usize,
}

impl BudgetedSearch {
    /// Start a search for `query` over `total_rows` rows from absolute row
    /// `base_row`. An invalid regex fails here, before any row is scanned.
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
            ambiguous_width: AmbiguousWidth::Single,
            cut_short_rows: 0,
        })
    }

    /// Measure match columns under the terminal's `ambiguous_width` policy, as
    /// [`SearchIndex::set_ambiguous_width`] does. Set it before feeding rows.
    #[must_use]
    pub fn with_ambiguous_width(mut self, ambiguous: AmbiguousWidth) -> Self {
        self.ambiguous_width = ambiguous;
        self
    }

    /// Consume and verify one row, absolute `base_row + rows_fed()`. Rows arrive in
    /// ascending order; feeding past the window is a no-op.
    #[cfg(test)]
    pub fn feed_row(&mut self, text: &str) {
        self.feed_row_cow(Cow::Borrowed(text), None);
    }

    /// Consume and verify one owned row; `text` is dropped before returning.
    pub fn feed_row_owned(&mut self, text: String) {
        self.feed_row_cow(Cow::Owned(text), None);
    }

    /// [`feed_row_owned`](Self::feed_row_owned) with the [`LineCells`] the text was
    /// read from; matches are reported in those cells.
    pub fn feed_row_cells(&mut self, text: String, cells: &LineCells) {
        self.feed_row_cow(Cow::Owned(text), Some(cells));
    }

    fn feed_row_cow(&mut self, text: Cow<'_, str>, cells: Option<&LineCells>) {
        if self.rows_fed >= self.total_rows {
            return;
        }
        let row_offset = self.rows_fed;
        let abs_row = self.base_row.saturating_add(row_offset);
        self.rows_fed += 1;
        // The batch index's one-time truncation warning, at its first eviction.
        if self.verify_from > 0 && row_offset == self.max_cached_lines {
            let lowest = self
                .base_row
                .saturating_add(final_evicted_prefix(self.rows_fed, self.max_cached_lines));
            aterm_log::warn!(
                "search past the {}-line cache; results incomplete below line {}",
                self.max_cached_lines,
                lowest,
            );
        }
        if row_offset < self.verify_from {
            return;
        }
        // Past the cap nothing can change the result (the batch path stops there
        // too); verifying only the retained suffix means eviction never removes a
        // capped match.
        if self.matches.len() < MAX_SEARCH_MATCHES {
            self.verify_row(abs_row, &text, cells);
        }
    }

    /// Rows consumed so far: the window-relative index of the next row.
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

    /// A full snapshot of the results (partial until [`is_complete`](Self::is_complete)),
    /// reported as [`SearchIndex::search_results_opts`] reports them. Resumable owners
    /// use [`results_range`](Self::results_range) to bound copying.
    #[must_use]
    pub fn results(&self) -> SearchResults {
        let capped = self.matches.len() >= MAX_SEARCH_MATCHES;
        let evicted = self.verify_from > 0;
        let lowest = self.final_lowest_retained_line;
        let matches = if evicted {
            self.matches
                .iter()
                .filter(|m| m.line >= lowest)
                .cloned()
                .collect()
        } else {
            self.matches.clone()
        };
        SearchResults::new(matches, evicted || capped, lowest)
            .with_cut_short_lines(self.cut_short_rows)
    }

    /// Number of matches accumulated so far, so an owner can deliver each once.
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.matches.len()
    }

    /// Clone at most `limit` matches from `start`, with metadata for the whole
    /// search. Verification starts at the final retained suffix, so a slice already
    /// handed out is never invalidated by later eviction.
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
            .with_cut_short_lines(self.cut_short_rows)
    }

    /// Append row `abs_row`'s matches to `self.matches`, up to the cap.
    fn verify_row(&mut self, abs_row: usize, text: &str, cells: Option<&LineCells>) {
        if self.query.is_empty() && !matches!(&self.matcher, RowMatcher::Regex(_)) {
            return;
        }
        let col_map = match cells {
            Some(cells) => ColumnMap::with_cells(text, cells),
            None => ColumnMap::new(text, self.ambiguous_width),
        };
        match &mut self.matcher {
            RowMatcher::Literal => {
                // The batch forward iterator's own scan; no postings for a row read once.
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
                let mut scan = scan_line(re, text);
                for cap in scan.by_ref() {
                    // Skip empty spans and zero-column spans, as the batch path does.
                    if cap.start() == cap.end() {
                        continue;
                    }
                    let (start_col, end_col) = col_map.span(cap.start(), cap.end());
                    if start_col == end_col {
                        continue;
                    }
                    self.matches
                        .push(SearchMatch::new(abs_row, start_col, end_col));
                    if self.matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
                if scan.step_limit_exceeded() {
                    self.cut_short_rows = self.cut_short_rows.saturating_add(1);
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

    /// Feed `rows` to `search` one row per call until it completes. A caller's
    /// budget only decides between which calls it stops, so this is every slicing.
    fn drive(mut search: BudgetedSearch, rows: &[&str]) -> SearchResults {
        while !search.is_complete() {
            search.feed_row(rows[search.rows_fed()]);
        }
        search.results()
    }

    /// Drive a budgeted search over `rows` at `base` to completion.
    fn budgeted(
        rows: &[&str],
        base: usize,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> SearchResults {
        let search = BudgetedSearch::new(query, case_sensitive, is_regex, base, rows.len())
            .expect("budgeted construction must succeed");
        drive(search, rows)
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
                // Spare capacity must not affect matching (reclamation is checked by
                // index_release_memory).
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

    /// `final_evicted_prefix` equals the real index's eviction schedule.
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

    /// The row feed equals the one-shot search, in every mode, at a non-zero base
    /// row.
    #[test]
    fn the_row_feed_equals_one_shot_across_all_modes() {
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
            let got = budgeted(&rows, 37, query, case_sensitive, is_regex);
            assert_eq!(
                got, oracle,
                "query={query:?} cs={case_sensitive} rx={is_regex}"
            );
        }
    }

    /// The match cap applies identically and is reported as incomplete.
    #[test]
    fn match_cap_matches_one_shot_and_reports_incomplete() {
        // 80 matches per row x 1300 rows = 104_000 > MAX_SEARCH_MATCHES.
        let row = "a".repeat(80);
        let rows: Vec<&str> = (0..1_300).map(|_| row.as_str()).collect();
        let oracle = one_shot(&rows, 0, "a", true, false);
        assert!(oracle.incomplete, "cap must be reported as incomplete");
        assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
        let got = budgeted(&rows, 0, "a", true, false);
        assert_eq!(got, oracle);
    }

    /// With eviction mid-build, the completed search equals the one-shot search over
    /// the same capped index.
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

        let search =
            BudgetedSearch::with_max_cached_lines("needle", true, false, 0, row_refs.len(), 10)
                .expect("budgeted construction");
        assert_eq!(drive(search, &row_refs), oracle);
    }

    /// Hitting the match cap in rows that are later evicted must not starve the
    /// retained suffix.
    #[test]
    fn combined_match_cap_and_eviction_equals_one_shot() {
        const CACHE_LINES: usize = 10;
        const ROWS: usize = 20;
        // Over MAX_SEARCH_MATCHES matches before the retained suffix, and matches
        // in every retained row.
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

        let search =
            BudgetedSearch::with_max_cached_lines("a", true, false, 0, rows.len(), CACHE_LINES)
                .expect("budgeted construction");
        assert_eq!(drive(search, &rows), oracle);
    }

    #[test]
    fn invalid_regex_fails_at_construction() {
        let err = BudgetedSearch::new("f(oo", false, true, 0, 10);
        assert!(matches!(err, Err(SearchOptionsError::InvalidRegex(_))));
    }

    /// A row whose regex scan runs out of budget is reported as the one-shot search
    /// reports it: matches found so far, counted in `cut_short_lines`, incomplete,
    /// later rows still searched. Cases: one search over budget, and a `find_iter`
    /// whose per-match searches each fit but together re-walk the row quadratically.
    #[test]
    fn a_row_the_regex_scan_cannot_finish_is_reported_like_the_one_shot() {
        let wide = format!("{}z", "x".repeat(4095));
        let run = "a".repeat(4096);
        for (pattern, row) in [("(?:x?){1020}z", wide.as_str()), (r"\w+=|\w", run.as_str())] {
            let rows = ["x z aa", row, "after z aa", "x z aa again"];
            let mut index = SearchIndex::new();
            for (i, text) in rows.iter().enumerate() {
                index.index_line(i, text);
            }
            let oracle = index
                .search_results_opts(pattern, true, true)
                .expect("a scan out of budget is reported, not refused");
            assert_eq!(oracle.cut_short_lines, 1, "{pattern:?}: {oracle:?}");
            assert!(oracle.incomplete, "{pattern:?}");
            assert!(
                oracle.matches.iter().any(|m| m.line == 2)
                    && oracle.matches.iter().any(|m| m.line == 3),
                "{pattern:?}: the rows after the long one are still searched: {oracle:?}"
            );
            let mut search =
                BudgetedSearch::new(pattern, true, true, 0, rows.len()).expect("compiles");
            search.feed_row(rows[0]);
            assert_eq!(
                search.results().cut_short_lines,
                0,
                "{pattern:?}: no row has run out yet"
            );
            for text in &rows[1..] {
                search.feed_row(text);
            }
            assert_eq!(search.results(), oracle, "{pattern:?}");
            assert_eq!(search.results_range(0, usize::MAX), oracle, "{pattern:?}");
        }
    }

    /// Partial results are a prefix of the final results; overfeeding is a no-op.
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

    /// Emitted range slices stay a prefix across an eviction boundary, and range
    /// reads are bounded.
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
