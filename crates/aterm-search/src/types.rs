// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared types for the search module.

/// A match found during search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Line number (0-indexed from oldest).
    pub line: usize,
    /// Starting column of the match (0-indexed).
    pub start_col: usize,
    /// Ending column of the match (exclusive).
    pub end_col: usize,
}

impl SearchMatch {
    /// Create a new search match.
    #[must_use]
    pub fn new(line: usize, start_col: usize, end_col: usize) -> Self {
        Self {
            line,
            start_col,
            end_col,
        }
    }

    /// Get the length of the match in columns.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Check if this is an empty match (zero length).
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.end_col <= self.start_col
    }
}

/// A set of search matches plus a signal for whether they are exhaustive.
///
/// The trigram index evicts the oldest lines once the cache cap is exceeded
/// (see [`SearchIndex::results_may_be_incomplete`]). When that has happened, a
/// plain `Vec<SearchMatch>` silently omits matches in evicted scrollback with
/// no signal to the caller. `SearchResults` carries that signal alongside the
/// matches so a consumer (e.g. the future `cmd_search`) can tell the AI that
/// results are truncated and report the still-searchable line range.
///
/// [`SearchIndex::results_may_be_incomplete`]: crate::SearchIndex::results_may_be_incomplete
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// The matches found, in the order produced by the search call.
    pub matches: Vec<SearchMatch>,
    /// True if eviction has dropped lines, so `matches` may be incomplete.
    pub incomplete: bool,
    /// The oldest line still retained in the index.
    ///
    /// Matches below this line have been evicted. When `incomplete` is false
    /// this is 0 and the full indexed range was searched.
    pub lowest_retained_line: usize,
}

impl SearchResults {
    /// Bundle `matches` with the eviction signal.
    #[must_use]
    pub fn new(matches: Vec<SearchMatch>, incomplete: bool, lowest_retained_line: usize) -> Self {
        Self {
            matches,
            incomplete,
            lowest_retained_line,
        }
    }

    /// Number of matches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// True if there are no matches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Direction for search iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchDirection {
    /// Search forward (oldest to newest).
    Forward,
    /// Search backward (newest to oldest).
    Backward,
}

/// One directed point-search: where it starts, which way it walks, and how
/// its edges behave. Bundled so every point-navigation entry point shares one
/// vocabulary instead of a growing positional-flag tail.
#[derive(Debug, Clone, Copy)]
pub struct DirectedFind {
    /// Absolute `(line, col)` anchor the walk measures from.
    pub anchor: (usize, usize),
    /// Which way to walk from the anchor.
    pub direction: SearchDirection,
    /// Whether a match exactly at the anchor qualifies.
    pub inclusive: bool,
    /// Whether to retry from the opposite buffer edge when the directional
    /// suffix/prefix has no match.
    pub wrap: bool,
}

/// Search result iterator (internal).
pub(super) enum SearchResult {
    None,
    All(std::ops::Range<u32>),
    /// Boxed to avoid large enum variant inflating all other variants.
    Bitmap(Box<crate::bitmap::SparseBitmapIntoIter>),
}

impl Iterator for SearchResult {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SearchResult::None => None,
            SearchResult::All(range) => range.next(),
            SearchResult::Bitmap(iter) => iter.next(),
        }
    }
}
