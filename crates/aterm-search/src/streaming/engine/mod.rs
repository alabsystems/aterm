// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Streaming search engine with memory-bounded results.

use super::types::*;

#[cfg(kani)]
type SeenPositions = Vec<(usize, usize)>;
#[cfg(not(kani))]
type SeenPositions = std::collections::HashSet<(usize, usize)>;

#[cfg(any(test, kani))]
mod invariants;
mod matching;
mod operations;

/// Streaming search engine with memory-bounded results.
///
/// Spec of record: `aterm_spec::derive::streaming_search_model()` (machine
/// `StreamingSearch`), the derived drift-free twin — see `streaming/mod.rs`.
///
/// ## Staleness Detection
///
/// A generation counter bumps on every content mutation (scan, add, invalidate,
/// cancel, pattern change). Consumers caching `StreamingMatch` coordinates
/// should snapshot [`generation()`](Self::generation) before using results and
/// discard cached coordinates when the generation changes (#7271).
#[derive(Debug)]
pub struct StreamingSearch {
    /// Current search state.
    state: SearchState,
    /// Filter mode.
    filter_mode: FilterMode,
    /// Current search pattern.
    pattern: String,
    /// Compiled regex (if filter mode is Regex).
    #[cfg(feature = "regex")]
    compiled_regex: Option<regex::Regex>,
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
    /// Monotonically increasing generation counter (#7271).
    ///
    /// Bumped on every mutation that changes results or invalidates coordinates.
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
            #[cfg(feature = "regex")]
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
    pub fn filter_mode(&self) -> FilterMode {
        self.filter_mode
    }

    /// Get the current pattern.
    #[must_use]
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

    /// Get the number of stored results.
    #[must_use]
    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    /// Check if wrap-around navigation is enabled.
    #[must_use]
    pub fn wrap_enabled(&self) -> bool {
        self.config.wrap_enabled
    }

    /// Check if case-sensitive matching is enabled.
    #[must_use]
    pub fn case_sensitive(&self) -> bool {
        self.config.case_sensitive
    }

    /// Check if all matches should be highlighted.
    #[must_use]
    pub fn highlight_all(&self) -> bool {
        self.config.highlight_all
    }

    /// Get the current generation counter.
    ///
    /// Bumped on every mutation that changes results or invalidates coordinates.
    /// Consumers should snapshot this before using results and discard cached
    /// coordinates when the generation changes (#7271).
    #[must_use]
    #[inline]
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
    /// Bump the generation counter (internal helper for #7271).
    #[inline]
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn record_seen_position(&mut self, pos_key: (usize, usize)) {
        #[cfg(kani)]
        {
            self.seen_positions.push(pos_key);
        }

        #[cfg(not(kani))]
        {
            self.seen_positions.insert(pos_key);
        }
    }

    /// Seed a valid terminal search state without exercising the full
    /// match-finding pipeline. Kani operation proofs use this to focus on
    /// post-search transitions like invalidation and pattern updates.
    ///
    /// The caller is responsible for providing unique, positive-width results
    /// and a valid 1-based `current_index`. Re-running `verify_all_invariants`
    /// here doubles the proof cost for the `proofs_gaps` harnesses and pushes
    /// CBMC into watchdog kills on otherwise concrete states (#6119).
    #[cfg(kani)]
    #[doc(hidden)]
    pub(crate) fn kani_seed_results(
        &mut self,
        pattern: &str,
        current_index: usize,
        results: &[(usize, usize, usize)],
    ) {
        self.pattern = pattern.to_string();
        self.filter_mode = FilterMode::Literal;
        self.state = if results.is_empty() {
            SearchState::NoResults
        } else {
            SearchState::HasResults
        };
        self.results.clear();
        self.seen_positions.clear();
        self.scan_progress = -1;
        self.total_matches = results.len();

        #[cfg(feature = "regex")]
        {
            self.compiled_regex = None;
        }

        for &(row, start_col, end_col) in results {
            self.record_seen_position((row, start_col));
            self.results
                .push(StreamingMatch::new(row, start_col, end_col));
        }

        self.current_index = current_index;
        debug_assert!(
            (self.results.is_empty() && self.current_index == 0)
                || (!self.results.is_empty()
                    && self.current_index >= 1
                    && self.current_index <= self.results.len()),
            "kani_seed_results requires a valid current_index",
        );
    }
}
