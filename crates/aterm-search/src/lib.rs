// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Trigram-indexed search with Bloom filter acceleration.

#![warn(missing_docs)]
#![cfg_attr(
    trust_verify,
    feature(custom_inner_attributes, register_tool),
    register_tool(trust)
)]

use std::borrow::Cow;

use aterm_types::text_shaping::AmbiguousWidth;

mod bitmap;
mod bloom;
mod budgeted;
mod bytesearch;
mod cells;
mod grapheme;
mod index;
mod iterators;
mod literal;
mod types;

pub use bloom::BloomFilter;
pub use budgeted::BudgetedSearch;
pub use cells::LineCells;
pub use grapheme::{display_columns, display_columns_with};
pub use index::{
    DEFAULT_MAX_CACHED_LINES, MAX_SEARCH_MATCHES, NarrowedSearch, SearchIndex, SearchOptionsError,
    max_cached_for_retained,
};
pub use types::{DirectedFind, LineFrame, SearchDirection, SearchMatch, SearchResults};

#[cfg(test)]
mod tests;

/// Search over a terminal's grid and scrollback ([`SearchIndex`]).
#[derive(Clone, Debug)]
pub struct TerminalSearch {
    /// Search index for all content.
    index: SearchIndex,
    /// Number of lines from scrollback that have been indexed.
    indexed_scrollback_lines: usize,
}

impl TerminalSearch {
    /// Create a new terminal search.
    #[must_use]
    pub fn new() -> Self {
        Self {
            index: SearchIndex::new(),
            indexed_scrollback_lines: 0,
        }
    }

    /// Create with expected capacity.
    #[must_use]
    pub fn with_capacity(expected_lines: usize) -> Self {
        let index = if expected_lines > BloomFilter::MAX_EFFECTIVE_CAPACITY {
            SearchIndex::with_capacity(BloomFilter::MAX_EFFECTIVE_CAPACITY)
        } else {
            SearchIndex::with_capacity(expected_lines)
        };
        Self {
            index,
            indexed_scrollback_lines: 0,
        }
    }

    /// Create with expected capacity and a cap on retained lines.
    #[must_use]
    pub fn with_capacity_and_max(expected_lines: usize, max_cached_lines: usize) -> Self {
        Self {
            index: SearchIndex::with_capacity_and_max(expected_lines, max_cached_lines),
            indexed_scrollback_lines: 0,
        }
    }

    /// Measure match columns under the terminal's `ambiguous_width` policy.
    #[must_use]
    pub fn with_ambiguous_width(mut self, ambiguous: AmbiguousWidth) -> Self {
        self.index.set_ambiguous_width(ambiguous);
        self
    }

    /// The `ambiguous_width` policy match columns are measured under.
    #[inline]
    #[must_use]
    pub const fn ambiguous_width(&self) -> AmbiguousWidth {
        self.index.ambiguous_width()
    }

    /// The oldest line still retained.
    #[inline]
    #[must_use]
    pub const fn lowest_retained_line(&self) -> usize {
        self.index.lowest_retained_line()
    }

    /// Whether results may be incomplete due to eviction.
    #[inline]
    #[must_use]
    pub const fn results_may_be_incomplete(&self) -> bool {
        self.index.results_may_be_incomplete()
    }

    /// Index a line pushed to scrollback.
    pub fn index_scrollback_line(&mut self, text: &str) {
        self.index.push_line(text);
        self.indexed_scrollback_lines = self.indexed_scrollback_lines.saturating_add(1);
    }

    /// Index multiple scrollback lines.
    #[cfg(test)]
    pub fn index_scrollback_lines(&mut self, lines: impl IntoIterator<Item = impl AsRef<str>>) {
        for line in lines {
            self.index_scrollback_line(line.as_ref());
        }
    }

    /// Re-index visible grid content, line `i` at row `base_line + i`.
    pub fn index_visible_content(
        &mut self,
        base_line: usize,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        for (offset, line) in lines.into_iter().enumerate() {
            self.index.index_line_cow(
                base_line.saturating_add(offset),
                Cow::Borrowed(line.as_ref()),
                None,
            );
        }
    }

    /// Index owned lines at absolute row numbers, moving each `String` into the cache.
    pub fn index_numbered_content_owned(
        &mut self,
        lines: impl IntoIterator<Item = (usize, String)>,
    ) {
        for (absolute_row, line) in lines {
            self.index
                .index_line_cow(absolute_row, Cow::Owned(line), None);
        }
    }

    /// Index owned grid rows at absolute row numbers ([`LineCells`]).
    pub fn index_numbered_cells_owned(
        &mut self,
        lines: impl IntoIterator<Item = (usize, String, LineCells)>,
    ) {
        for (absolute_row, line, cells) in lines {
            self.index
                .index_line_cow(absolute_row, Cow::Owned(line), Some(&cells));
        }
    }

    /// Record whether the bulk snapshot just built omitted an older history prefix.
    pub fn set_history_prefix_eviction(&mut self, lowest_retained_line: Option<usize>) {
        self.index.set_history_prefix_eviction(lowest_retained_line);
    }

    /// Drop rows the terminal no longer retains ([`SearchIndex::drop_history_below`]).
    pub fn drop_history_below(&mut self, first_retained_line: usize) {
        self.index.drop_history_below(first_retained_line);
    }

    /// Check if a query might have matches.
    #[must_use]
    #[cfg(test)]
    pub fn might_contain(&self, query: &str) -> bool {
        self.index.might_contain(query)
    }

    /// Search for a query string.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        self.index.search_with_positions(query)
    }

    /// Search with options ([`SearchIndex::search_results_opts`]).
    pub fn search_results_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<SearchResults, SearchOptionsError> {
        self.index
            .search_results_opts(query, case_sensitive, is_regex)
    }

    /// Search with options, keeping the capped edge for `direction` ([`MAX_SEARCH_MATCHES`]).
    pub fn search_results_opts_direction(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        direction: SearchDirection,
    ) -> Result<SearchResults, SearchOptionsError> {
        self.index
            .search_results_opts_direction(query, case_sensitive, is_regex, direction)
    }

    /// Cheap upper bound on the index's candidate count for a literal query.
    #[must_use]
    pub fn literal_candidate_bound(&self, query: &str, case_sensitive: bool) -> Option<u64> {
        self.index.literal_candidate_bound(query, case_sensitive)
    }

    /// One incremental-search narrowing step ([`SearchIndex::search_literal_narrowed`]).
    #[must_use]
    pub fn search_literal_narrowed(
        &self,
        query: &str,
        case_sensitive: bool,
        prev_lines: Option<&LineFrame>,
    ) -> NarrowedSearch {
        self.index
            .search_literal_narrowed(query, case_sensitive, prev_lines)
    }

    /// Find the next match after the given position.
    #[cfg(test)]
    pub fn find_next(
        &self,
        query: &str,
        after_line: usize,
        after_col: usize,
    ) -> Option<SearchMatch> {
        self.index
            .search_from_line(query, after_line)
            .find(|m| m.line > after_line || (m.line == after_line && m.start_col > after_col))
    }

    /// Find the next match with case-sensitivity and regex options.
    #[cfg(test)]
    pub fn find_next_opts(
        &self,
        query: &str,
        after_line: usize,
        after_col: usize,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        self.find_direction_opts(
            query,
            case_sensitive,
            is_regex,
            DirectedFind {
                anchor: (after_line, after_col),
                direction: SearchDirection::Forward,
                inclusive: false,
                wrap: false,
            },
        )
    }

    /// Find the previous match before the given position.
    #[cfg(test)]
    pub fn find_prev(
        &self,
        query: &str,
        before_line: usize,
        before_col: usize,
    ) -> Option<SearchMatch> {
        let exclusive_upper = before_line.saturating_add(1);
        self.index
            .search_before_line(query, exclusive_upper)
            .find(|m| m.line < before_line || (m.line == before_line && m.start_col < before_col))
    }

    /// Find the previous match with case-sensitivity and regex options.
    #[cfg(test)]
    pub fn find_prev_opts(
        &self,
        query: &str,
        before_line: usize,
        before_col: usize,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        self.find_direction_opts(
            query,
            case_sensitive,
            is_regex,
            DirectedFind {
                anchor: (before_line, before_col),
                direction: SearchDirection::Backward,
                inclusive: false,
                wrap: false,
            },
        )
    }

    /// Find one match from an absolute anchor in `direction`, unaffected by [`MAX_SEARCH_MATCHES`].
    pub fn find_direction_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
        find: DirectedFind,
    ) -> Result<Option<SearchMatch>, SearchOptionsError> {
        let DirectedFind {
            anchor: (anchor_line, anchor_col),
            direction,
            inclusive,
            wrap,
        } = find;
        if is_regex {
            return self.index.find_regex_direction(query, case_sensitive, find);
        }
        let find = |line: usize, col: usize, include: bool| {
            if case_sensitive {
                let found = match direction {
                    SearchDirection::Forward => {
                        self.index.search_from_line(query, line).find(|m| {
                            m.line > line
                                || (m.line == line
                                    && if include {
                                        m.start_col >= col
                                    } else {
                                        m.start_col > col
                                    })
                        })
                    }
                    SearchDirection::Backward => self
                        .index
                        .search_before_line(query, line.saturating_add(1))
                        .find(|m| {
                            m.line < line
                                || (m.line == line
                                    && if include {
                                        m.start_col <= col
                                    } else {
                                        m.start_col < col
                                    })
                        }),
                };
                return Ok(found);
            }
            Ok(match direction {
                SearchDirection::Forward => self
                    .index
                    .find_next_case_insensitive_from(query, line, col, include),
                SearchDirection::Backward => self
                    .index
                    .find_prev_case_insensitive_from(query, line, col, include),
            })
        };

        let found = find(anchor_line, anchor_col, inclusive)?;
        if found.is_some() || !wrap {
            return Ok(found);
        }
        match direction {
            SearchDirection::Forward => find(self.lowest_retained_line(), 0, true),
            SearchDirection::Backward => find(
                self.indexed_line_count().saturating_sub(1),
                usize::MAX,
                true,
            ),
        }
    }

    /// Get the number of indexed lines.
    #[inline]
    #[must_use]
    #[doc(hidden)]
    pub const fn indexed_line_count(&self) -> usize {
        self.index.len()
    }

    /// Get the number of scrollback lines indexed.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn indexed_scrollback_count(&self) -> usize {
        self.indexed_scrollback_lines
    }

    /// Clear the search index.
    pub fn clear(&mut self) {
        self.index.clear();
        self.indexed_scrollback_lines = 0;
    }

    /// Like [`clear`](Self::clear), but returns the grown map and vector capacity to
    /// the allocator (idle eviction).
    pub fn release(&mut self) {
        self.index.release();
        self.indexed_scrollback_lines = 0;
    }
}

impl Default for TerminalSearch {
    fn default() -> Self {
        Self::new()
    }
}
