// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core types for streaming search.

/// Search state (from TLA+ SearchStates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchState {
    /// No active search.
    Idle,
    /// Currently scanning rows.
    Searching,
    /// Search completed with results.
    HasResults,
    /// Search completed with no results.
    NoResults,
}

/// Filter mode for pattern matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FilterMode {
    /// Exact literal string match.
    #[default]
    Literal,
    /// Regular expression match.
    Regex,
    /// Fuzzy/approximate match.
    Fuzzy,
}

/// Navigation direction for search iteration.
///
/// Renamed from `Direction` to `NavigationDirection` to distinguish from
/// bidi `Direction` type (see #1203).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub(crate) enum NavigationDirection {
    /// Navigate forward (toward newer matches).
    #[default]
    Forward,
    /// Navigate backward (toward older matches).
    Backward,
}

/// A match record (from TLA+ Match type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingMatch {
    /// Row index (0-indexed from oldest).
    pub row: usize,
    /// Starting column (0-indexed).
    pub start_col: usize,
    /// Ending column (exclusive).
    pub end_col: usize,
    /// Match length in characters.
    pub match_len: usize,
}

impl StreamingMatch {
    /// Create a new match record.
    #[must_use]
    pub fn new(row: usize, start_col: usize, end_col: usize) -> Self {
        let match_len = end_col.saturating_sub(start_col);
        Self {
            row,
            start_col,
            end_col,
            match_len,
        }
    }

    /// Check if this match overlaps with another at the same position.
    #[cfg(any(test, kani))]
    #[must_use]
    pub(crate) fn same_position(&self, other: &Self) -> bool {
        self.row == other.row && self.start_col == other.start_col
    }
}

/// Streaming search configuration.
#[derive(Debug, Clone)]
pub struct StreamingSearchConfig {
    /// Maximum number of stored results (memory bound).
    pub max_results: usize,
    /// Maximum pattern length.
    pub max_pattern_len: usize,
    /// Enable wraparound navigation.
    pub wrap_enabled: bool,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Highlight all matches (vs just current).
    pub highlight_all: bool,
}

impl Default for StreamingSearchConfig {
    fn default() -> Self {
        Self {
            max_results: 10_000,
            max_pattern_len: 1024,
            wrap_enabled: true,
            case_sensitive: cfg!(kani),
            highlight_all: true,
        }
    }
}
