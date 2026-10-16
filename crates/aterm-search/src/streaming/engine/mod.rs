// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Streaming search engine with memory-bounded results.

use super::types::*;

type SeenPositions = std::collections::HashSet<(usize, usize)>;

#[cfg(test)]
mod invariants;
mod matching;
mod operations;

/// Streaming search engine with memory-bounded results.
///
/// Spec of record: `aterm_spec::derive::streaming_search_model()` (see
/// `streaming/mod.rs`).
#[derive(Debug)]
pub struct StreamingSearch {
    /// Current search state.
    state: SearchState,
    /// Filter mode.
    filter_mode: FilterMode,
    /// Current search pattern.
    pattern: String,
    /// Compiled regex (if filter mode is Regex).
    compiled_regex: Option<aterm_regex::Regex>,
    /// Search results (bounded by max_results).
    results: Vec<StreamingMatch>,
    /// Current highlighted result index (1-based, 0 = none).
    current_index: usize,
    /// Row currently being scanned (-1 = not scanning).
    scan_progress: isize,
    /// Total matches found (may exceed stored results).
    total_matches: usize,
    /// Configuration.
    config: StreamingSearchConfig,
    /// Deduplication set (row, start_col).
    seen_positions: SeenPositions,
    /// Bumped when a search starts, restarts or is cancelled, and when a content change
    /// alters the results; scanning only appends.
    generation: u64,
}

impl StreamingSearch {
    /// Create a new streaming search engine.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(StreamingSearchConfig::default())
    }

    /// Create with custom configuration.
    #[must_use]
    pub fn with_config(config: StreamingSearchConfig) -> Self {
        Self {
            state: SearchState::Idle,
            filter_mode: FilterMode::Literal,
            pattern: String::new(),
            compiled_regex: None,
            results: Vec::new(),
            current_index: 0,
            scan_progress: -1,
            total_matches: 0,
            config,
            seen_positions: SeenPositions::default(),
            generation: 0,
        }
    }

    /// Get the current search state.
    #[must_use]
    pub fn state(&self) -> SearchState {
        self.state
    }

    /// Get the current filter mode.
    #[must_use]
    #[cfg(test)]
    pub fn filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    /// Get the current pattern.
    #[must_use]
    #[cfg(test)]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Get the search results.
    #[must_use]
    pub fn results(&self) -> &[StreamingMatch] {
        &self.results
    }

    /// Get the current match index (1-based, 0 = none).
    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Get the currently highlighted match.
    #[must_use]
    #[cfg(test)]
    pub fn current_match(&self) -> Option<&StreamingMatch> {
        if self.current_index > 0 && self.current_index <= self.results.len() {
            Some(&self.results[self.current_index - 1])
        } else {
            None
        }
    }

    /// Get the scan progress (row being scanned, -1 if not scanning).
    #[must_use]
    pub fn scan_progress(&self) -> isize {
        self.scan_progress
    }

    /// Get the total number of matches found (may exceed stored).
    #[must_use]
    pub fn total_matches(&self) -> usize {
        self.total_matches
    }

    /// Whether the current search has omitted matches because its result cap
    /// or a regex scan budget was exhausted. A finished search with no results
    /// is not a definitive miss when this is true.
    ///
    /// Regex exhaustion is conservative and sticky for the current compiled
    /// pattern, including content changes and reflows. [`start_search`](Self::start_search)
    /// starts a fresh scan and clears that history.
    #[must_use]
    pub fn incomplete(&self) -> bool {
        self.total_matches > self.results.len()
            || (self.filter_mode == FilterMode::Regex
                && self
                    .compiled_regex
                    .as_ref()
                    .is_some_and(aterm_regex::Regex::step_limit_exceeded))
    }

    /// Get the number of stored results.
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    /// Check if wrap-around navigation is enabled.
    #[must_use]
    #[cfg(test)]
    pub fn wrap_enabled(&self) -> bool {
        self.config.wrap_enabled
    }

    /// Check if case-sensitive matching is enabled.
    #[must_use]
    #[cfg(test)]
    pub fn case_sensitive(&self) -> bool {
        self.config.case_sensitive
    }

    /// Check if all matches should be highlighted.
    #[must_use]
    #[cfg(test)]
    pub fn highlight_all(&self) -> bool {
        self.config.highlight_all
    }

    /// Get the current generation counter.
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Default for StreamingSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingSearch {
    /// Bump the generation counter.
    #[inline]
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn record_seen_position(&mut self, pos_key: (usize, usize)) {
        {
            self.seen_positions.insert(pos_key);
        }
    }
}
