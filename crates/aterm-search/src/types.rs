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
    #[inline]
    #[must_use]
    pub const fn new(line: usize, start_col: usize, end_col: usize) -> Self {
        Self {
            line,
            start_col,
            end_col,
        }
    }

    /// Get the length of the match in columns.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.end_col.saturating_sub(self.start_col)
    }

    /// Check if this is an empty match (zero length).
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.end_col <= self.start_col
    }
}

/// A set of search matches plus a signal for whether they are exhaustive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResults {
    /// The matches found, in the order produced by the search call.
    pub matches: Vec<SearchMatch>,
    /// True if eviction has dropped lines, so `matches` may be incomplete.
    pub incomplete: bool,
    /// The oldest line still retained in the index.
    pub lowest_retained_line: usize,
    /// How many lines were not searched to their end.
    pub cut_short_lines: usize,
}

impl SearchResults {
    /// Bundle `matches` with the eviction signal.
    #[inline]
    #[must_use]
    pub const fn new(
        matches: Vec<SearchMatch>,
        incomplete: bool,
        lowest_retained_line: usize,
    ) -> Self {
        Self {
            matches,
            incomplete,
            lowest_retained_line,
            cut_short_lines: 0,
        }
    }

    /// Record `lines` lines not searched to their end, which makes the results incomplete when
    /// there are any.
    #[inline]
    #[must_use]
    pub const fn with_cut_short_lines(mut self, lines: usize) -> Self {
        self.cut_short_lines = lines;
        self.incomplete |= lines > 0;
        self
    }

    /// True if there are no matches.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// An ascending, deduplicated set of line numbers, stored as `u32` offsets from a base line so a
/// frame costs four bytes a line however large the line numbers are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineFrame {
    /// The line offset 0 stands for.
    base: usize,
    /// Ascending offsets from `base`.
    offsets: Vec<u32>,
}

impl LineFrame {
    /// An empty frame whose offsets count from `base`.
    #[inline]
    pub(crate) const fn with_base(base: usize) -> Self {
        Self {
            base,
            offsets: Vec::new(),
        }
    }

    /// Append the line `base + offset`; offsets arrive ascending.
    #[inline]
    pub(crate) fn push_offset(&mut self, offset: u32) {
        debug_assert!(self.offsets.last().is_none_or(|&last| last < offset));
        self.offsets.push(offset);
    }

    /// How many lines the frame holds.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.offsets.len()
    }

    /// True if the frame holds no lines.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// The lines, ascending.
    #[inline]
    pub fn lines(&self) -> impl Iterator<Item = usize> + '_ {
        let base = self.base;
        self.offsets
            .iter()
            .map(move |&offset| base.saturating_add(offset as usize))
    }

    /// Whether `line` is in the frame.
    #[inline]
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

/// One directed point-search: where it starts, which way it walks, and how its edges behave.
#[derive(Debug, Clone, Copy)]
pub struct DirectedFind {
    /// Absolute `(line, col)` anchor the walk measures from.
    pub anchor: (usize, usize),
    /// Which way to walk from the anchor.
    pub direction: SearchDirection,
    /// Whether a match exactly at the anchor qualifies.
    pub inclusive: bool,
    /// Whether to retry from the opposite buffer edge when the directional suffix/prefix has no
    /// match.
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
