// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-buffer search.

use std::borrow::Cow;

use aterm_types::text_shaping::AmbiguousWidth;

use crate::cells::LineCells;
use crate::grapheme::ColumnMap;
use crate::index::{
    CaseInsensitiveMatcher, MAX_SEARCH_MATCHES, SearchIndex, SearchOptionsError, scan_line,
};
use crate::iterators::next_literal_match;
use crate::types::{SearchMatch, SearchResults};

enum RowMatcher {
    Literal,
    CaseInsensitive(CaseInsensitiveMatcher),
    Regex(aterm_regex::Regex),
}

/// A budgeted, resumable search over the absolute rows `base_row, base_row + total_rows)`, fed one
/// row at a time in ascending order. See the module docs for the results-equality contract.
pub struct BudgetedSearch {
    query: String,
    matcher: RowMatcher,
    matches: Vec<SearchMatch>,
    base_row: usize,
    total_rows: usize,
    rows_fed: usize,
    verify_from: usize,
    final_lowest_retained_line: usize,
    ambiguous_width: AmbiguousWidth,
    cut_short_rows: usize,
}

impl BudgetedSearch {
    /// Start a search for `query` over `total_rows` rows from absolute row `base_row`.
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

    /// [`new`](Self::new) with an explicit depth window.
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
        let verify_from = total_rows.saturating_sub(max_cached_lines.max(1));
        let final_lowest_retained_line = if verify_from > 0 {
            base_row.saturating_add(verify_from)
        } else {
            0
        };
        Ok(Self {
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
    /// [`SearchIndex::set_ambiguous_width`] does.
    #[inline]
    #[must_use]
    pub const fn with_ambiguous_width(mut self, ambiguous: AmbiguousWidth) -> Self {
        self.ambiguous_width = ambiguous;
        self
    }

    /// Consume and verify one row, absolute `base_row + rows_fed()`.
    #[cfg(test)]
    pub fn feed_row(&mut self, text: &str) {
        self.feed_row_cow(Cow::Borrowed(text), None);
    }

    /// Consume and verify one owned row; `text` is dropped before returning.
    pub fn feed_row_owned(&mut self, text: String) {
        self.feed_row_cow(Cow::Owned(text), None);
    }

    /// [`feed_row_owned`](Self::feed_row_owned) with the [`LineCells`] the text was read from;
    /// matches are reported in those cells.
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
        if row_offset < self.verify_from {
            return;
        }
        let truncated = cells.is_some_and(LineCells::is_truncated);
        if truncated {
            self.cut_short_rows = self.cut_short_rows.saturating_add(1);
        }
        if self.matches.len() < MAX_SEARCH_MATCHES {
            self.verify_row(abs_row, &text, cells, truncated);
        }
    }

    /// Rows consumed so far: the window-relative index of the next row.
    #[inline]
    #[must_use]
    pub const fn rows_fed(&self) -> usize {
        self.rows_fed
    }

    /// Total rows in the search window.
    #[inline]
    #[must_use]
    pub const fn total_rows(&self) -> usize {
        self.total_rows
    }

    /// Whether every row in the window has been fed.
    #[inline]
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.rows_fed >= self.total_rows
    }

    /// A full snapshot of the results (partial until [`is_complete`](Self::is_complete)), reported
    /// as [`SearchIndex::search_results_opts`] reports them.
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
    #[inline]
    #[must_use]
    pub const fn result_count(&self) -> usize {
        self.matches.len()
    }

    /// Clone at most `limit` matches from `start`, with metadata for the whole search.
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

    fn verify_row(
        &mut self,
        abs_row: usize,
        text: &str,
        cells: Option<&LineCells>,
        truncated: bool,
    ) {
        if self.query.is_empty() && !matches!(&self.matcher, RowMatcher::Regex(_)) {
            return;
        }
        let col_map = match cells {
            Some(cells) => ColumnMap::with_cells(text, cells),
            None => ColumnMap::new(text, self.ambiguous_width),
        };
        match &mut self.matcher {
            RowMatcher::Literal => {
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
                if scan.step_limit_exceeded() && !truncated {
                    self.cut_short_rows = self.cut_short_rows.saturating_add(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn depth_window_index(
        rows: &[&str],
        cells: Option<&[LineCells]>,
        base: usize,
        cap: usize,
    ) -> SearchIndex {
        let skipped = rows.len().saturating_sub(cap);
        let mut index = SearchIndex::with_capacity_and_max(rows.len() - skipped, cap);
        for (i, row) in rows.iter().enumerate().skip(skipped) {
            index.index_line_cow(base + i, Cow::Borrowed(row), cells.map(|cells| &cells[i]));
        }
        index.set_history_prefix_eviction((skipped > 0).then_some(base + skipped));
        index
    }

    fn drive(mut search: BudgetedSearch, rows: &[&str]) -> SearchResults {
        while !search.is_complete() {
            search.feed_row(rows[search.rows_fed()]);
        }
        search.results()
    }

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

    #[test]
    fn past_its_cap_the_search_equals_the_depth_window_not_the_eviction_schedule() {
        let rows: Vec<String> = (0..100).map(|i| format!("needle row {i}")).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let oracle = depth_window_index(&rows, None, 5, 10)
            .search_results_opts("needle", true, false)
            .expect("oracle search");
        assert!(
            oracle.incomplete,
            "the rows past the window are not searched"
        );
        assert_eq!(oracle.lowest_retained_line, 5 + 90);
        assert_eq!(oracle.matches.len(), 10, "every row of the window");
        let mut search = BudgetedSearch::with_max_cached_lines("needle", true, false, 5, 100, 10)
            .expect("budgeted construction");
        for row in &rows {
            search.feed_row(row);
        }
        assert_eq!(search.results(), oracle);
        let mut evicting = SearchIndex::with_max_cached_lines(10);
        for (i, row) in rows.iter().enumerate() {
            evicting.index_line(5 + i, row);
        }
        let scheduled = evicting
            .search_results_opts("needle", true, false)
            .expect("search");
        assert_eq!(
            scheduled.lowest_retained_line,
            5 + 92,
            "the low-water schedule"
        );
        assert_ne!(scheduled, oracle);
    }

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

    #[test]
    fn match_cap_matches_one_shot_and_reports_incomplete() {
        let row = "a".repeat(80);
        let rows: Vec<&str> = (0..1_300).map(|_| row.as_str()).collect();
        let oracle = one_shot(&rows, 0, "a", true, false);
        assert!(oracle.incomplete, "cap must be reported as incomplete");
        assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
        let got = budgeted(&rows, 0, "a", true, false);
        assert_eq!(got, oracle);
    }

    #[test]
    fn combined_match_cap_and_depth_window_equals_one_shot() {
        const CACHE_LINES: usize = 10;
        const ROWS: usize = 20;
        let dense = "a".repeat(MAX_SEARCH_MATCHES / 2 + 1);
        let rows: Vec<&str> = (0..ROWS).map(|_| dense.as_str()).collect();

        let oracle = depth_window_index(&rows, None, 0, CACHE_LINES)
            .search_results_opts("a", true, false)
            .expect("oracle search");
        assert!(
            oracle.incomplete,
            "cap plus the depth window must be reported"
        );
        assert_eq!(oracle.matches.len(), MAX_SEARCH_MATCHES);
        assert!(
            oracle
                .matches
                .iter()
                .all(|m| m.line >= oracle.lowest_retained_line),
            "oracle only returns the window"
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

    #[test]
    fn a_row_read_short_of_its_end_is_reported_like_the_one_shot() {
        let wide = "x".repeat(4096);
        let rows = [
            "x z aa",
            wide.as_str(),
            "after z aa",
            "x z aa again",
            "z tail",
        ];
        let cells: Vec<LineCells> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut cells = LineCells::identity(row.len());
                if i == 1 {
                    cells.mark_truncated();
                }
                cells
            })
            .collect();
        for (pattern, case_sensitive, is_regex, cap, cut_short) in [
            ("aa", true, false, 100, 1),
            ("AA", false, false, 100, 1),
            ("z", true, true, 100, 1),
            ("(?:x?){1020}z", true, true, 100, 1),
            ("z", true, true, 3, 0),
        ] {
            let oracle = depth_window_index(&rows, Some(&cells), 0, cap)
                .search_results_opts(pattern, case_sensitive, is_regex)
                .expect("searched");
            assert_eq!(oracle.cut_short_lines, cut_short, "{pattern:?}: {oracle:?}");
            let mut search = BudgetedSearch::with_max_cached_lines(
                pattern,
                case_sensitive,
                is_regex,
                0,
                rows.len(),
                cap,
            )
            .expect("compiles");
            for (row, cells) in rows.iter().zip(&cells) {
                search.feed_row_cells((*row).to_string(), cells);
            }
            assert_eq!(search.results(), oracle, "{pattern:?} cap {cap}");
        }
    }

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

    #[test]
    fn result_ranges_are_bounded_and_stable_past_the_cap() {
        let rows: Vec<String> = (0..20).map(|i| format!("needle row {i}")).collect();
        let mut search =
            BudgetedSearch::with_max_cached_lines("needle", true, false, 0, rows.len(), 10)
                .expect("construction");

        for row in rows.iter().take(14) {
            search.feed_row(row);
        }
        let first = search.results_range(0, 2);
        assert!(first.matches.len() <= 2);
        assert!(first.matches.iter().all(|m| m.line >= 10));
        assert!(first.incomplete);
        assert_eq!(first.lowest_retained_line, 10);

        for row in rows.iter().skip(14) {
            search.feed_row(row);
        }
        let final_results = search.results();
        assert_eq!(
            &final_results.matches[..first.matches.len()],
            first.matches.as_slice(),
            "an emitted delta must remain a prefix of the finished search"
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
