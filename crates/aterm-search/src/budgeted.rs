// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Budgeted, resumable full-buffer search.

use std::borrow::Cow;
use std::cell::OnceCell;

use aterm_types::text_shaping::AmbiguousWidth;

use crate::cells::LineCells;
use crate::grapheme::{ColumnMap, SearchColumns};
use crate::index::{
    CaseInsensitiveMatcher, MAX_SEARCH_MATCHES, SearchIndex, SearchOptionsError, scan_line,
};
use crate::iterators::literal_occurrence;
use crate::types::{SearchMatch, SearchResults};

enum RowMatcher {
    Literal(crate::bytesearch::OwnedSearcher),
    CaseInsensitive(CaseInsensitiveMatcher),
    Regex(aterm_regex::Regex),
}

/// A consumed row needs column projection only if its text has an occurrence.
struct DeferredColumns<'a> {
    text: &'a str,
    cells: Option<&'a LineCells>,
    ambiguous_width: AmbiguousWidth,
    map: OnceCell<ColumnMap>,
}

impl SearchColumns for DeferredColumns<'_> {
    #[inline]
    fn is_ascii(&self) -> bool {
        self.text.is_ascii()
    }

    #[inline]
    fn get(&self) -> &ColumnMap {
        self.map.get_or_init(|| match self.cells {
            Some(cells) => ColumnMap::with_cells(self.text, cells),
            None => ColumnMap::new(self.text, self.ambiguous_width),
        })
    }
}

/// A budgeted, resumable search over the absolute rows `[base_row, base_row + total_rows)`, fed
/// one row at a time in ascending order.
pub struct BudgetedSearch {
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
            RowMatcher::Literal(crate::bytesearch::OwnedSearcher::new(query))
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
        let empty = match &self.matcher {
            RowMatcher::Literal(owned) => owned.searcher().needle().is_empty(),
            RowMatcher::CaseInsensitive(matcher) => matcher.is_empty(),
            RowMatcher::Regex(_) => false,
        };
        if empty {
            return;
        }
        let col_map = DeferredColumns {
            text,
            cells,
            ambiguous_width: self.ambiguous_width,
            map: OnceCell::new(),
        };
        match &mut self.matcher {
            RowMatcher::Literal(owned) => {
                let searcher = owned.searcher();
                let mut positions = searcher.matches_in::<false>(text.as_bytes());
                let Some(mut at) = positions.next() else {
                    return;
                };
                let col_map = col_map.get();
                loop {
                    let found = literal_occurrence(abs_row, at, searcher.needle().len(), col_map);
                    if found.start_col != found.end_col {
                        self.matches.push(found);
                        if self.matches.len() >= MAX_SEARCH_MATCHES {
                            break;
                        }
                    }
                    let Some(next) = positions.next() else {
                        break;
                    };
                    at = next;
                }
            }
            RowMatcher::CaseInsensitive(matcher) => {
                let matches = &mut self.matches;
                matcher.visit_matches::<false>(abs_row, text, &col_map, |found| {
                    matches.push(found);
                    matches.len() < MAX_SEARCH_MATCHES
                });
            }
            RowMatcher::Regex(re) => {
                let mut scan = scan_line(re, text);
                let mut ready = None;
                for cap in scan.by_ref() {
                    if cap.start() == cap.end() {
                        continue;
                    }
                    let columns = ready.get_or_insert_with(|| col_map.get());
                    let (start_col, end_col) = columns.span(cap.start(), cap.end());
                    if start_col != end_col {
                        self.matches
                            .push(SearchMatch::new(abs_row, start_col, end_col));
                        if self.matches.len() >= MAX_SEARCH_MATCHES {
                            break;
                        }
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
#[path = "budgeted_tests.rs"]
mod tests;
