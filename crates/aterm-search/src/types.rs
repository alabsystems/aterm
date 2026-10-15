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
    /// How many lines a regex scan could not finish within the line's scan
    /// budget — a line long enough, for a pattern costly enough, that the scan
    /// stopped part-way (a soft-wrapped run of megabytes, say). The matches it
    /// found before it stopped are in `matches`; any after that point on that
    /// line are missing. Non-zero implies `incomplete`. Always 0 for a literal
    /// search.
    pub cut_short_lines: usize,
}

impl SearchResults {
    /// Bundle `matches` with the eviction signal.
    #[must_use]
    pub fn new(matches: Vec<SearchMatch>, incomplete: bool, lowest_retained_line: usize) -> Self {
        Self {
            matches,
            incomplete,
            lowest_retained_line,
            cut_short_lines: 0,
        }
    }

    /// Record `lines` lines a regex scan could not finish, which makes the
    /// results incomplete when there are any.
    #[must_use]
    pub fn with_cut_short_lines(mut self, lines: usize) -> Self {
        self.cut_short_lines = lines;
        self.incomplete |= lines > 0;
        self
    }

    /// True if there are no matches.
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// An ascending, deduplicated set of line numbers, stored as `u32` offsets
/// from a base line so a frame costs four bytes a line however large the line
/// numbers are.
///
/// Line numbers are absolute terminal rows, which pass `u32::MAX` in a
/// long-lived tab (R15 content-1), so a frame cannot hold them as `u32`
/// directly. It carries its own base rather than borrowing an index's: an
/// index re-keys itself as its window moves, and a frame handed out by one
/// index state must still name the same lines when it comes back
/// ([`SearchIndex::search_literal_narrowed`](crate::SearchIndex::search_literal_narrowed)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineFrame {
    /// The line offset 0 stands for.
    base: usize,
    /// Ascending offsets from `base`.
    offsets: Vec<u32>,
}

impl LineFrame {
    /// An empty frame whose offsets count from `base`.
    pub(crate) fn with_base(base: usize) -> Self {
        Self {
            base,
            offsets: Vec::new(),
        }
    }

    /// Append the line `base + offset`; offsets arrive ascending.
    pub(crate) fn push_offset(&mut self, offset: u32) {
        debug_assert!(self.offsets.last().is_none_or(|&last| last < offset));
        self.offsets.push(offset);
    }

    /// How many lines the frame holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// True if the frame holds no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The lines, ascending.
    pub fn lines(&self) -> impl Iterator<Item = usize> + '_ {
        let base = self.base;
        self.offsets
            .iter()
            .map(move |&offset| base.saturating_add(offset as usize))
    }

    /// Whether `line` is in the frame.
    #[must_use]
    pub fn contains(&self, line: usize) -> bool {
        line.checked_sub(self.base)
            .and_then(|offset| u32::try_from(offset).ok())
            .is_some_and(|offset| self.offsets.binary_search(&offset).is_ok())
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
#[cfg(test)]
pub(super) enum SearchResult {
    None,
    All(std::ops::Range<u32>),
    /// Boxed to avoid large enum variant inflating all other variants.
    Bitmap(Box<crate::bitmap::SparseBitmapIntoIter>),
}

#[cfg(test)]
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
