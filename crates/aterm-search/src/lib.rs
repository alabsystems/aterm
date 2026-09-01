// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Trigram-indexed search with Bloom filter acceleration.
//!
//! ## Design
//!
//! - Bloom filter for fast negative lookups (O(1) per trigram check)
//! - Trigram index for candidate filtering via posting-list intersection
//! - SparseBitmap (ascending sorted Vec) for line number storage
//! - Generic interfaces for integrating with grid/scrollback providers
//!
//! ## Lifecycle-Driven Document Identity (E2 redesign)
//!
//! [`LifecycleSearchIndex`] wraps the trigram engine behind compact per-epoch
//! document ids and an explicit grid→index lifecycle event alphabet
//! ([`SearchLifecycleEvent`]): append/replace/evict/reflow/clear/alt-screen.
//! Gated by a differential equivalence oracle against the legacy
//! absolute-row-keyed path (`lifecycle_oracle_tests.rs`).
//!
//! ## Streaming Search
//!
//! The [`streaming`] module provides memory-bounded streaming search:
//! - Search through content incrementally (row by row)
//! - Memory-bounded results with configurable limits
//! - Multiple filter modes: Literal, Regex, Fuzzy
//! - Navigation with optional wraparound
//!
//! ## Performance
//!
//! | Operation | Time Complexity |
//! |-----------|-----------------|
//! | Negative lookup | O(t) bloom filter checks, t = query trigrams |
//! | Candidate search | O(t) posting-list intersections (SparseBitmap) |
//! | Verified search | O(t + k·L) where k = candidates, L = avg line length |
//! | Index line | O(n) where n = line length |
//!
//! Complexity claims derived from [`SearchIndex::search`] and
//! [`SearchIndex::search_with_positions`]. Bloom filter O(1)
//! per-check bound verified by operation counters in `bloom` module tests.
//!
//! ## Verification
//!
//! - Kani proofs: `no_false_negatives_symbolic`
//! - Tests: `search_with_positions`, `search_reverse_iterator_multiple_matches_per_line`
//! - Fuzz tests: `fuzz/fuzz_targets/search.rs` (in aterm-core)
//! - TLA+ spec: derived `StreamingSearch` machine
//!   (`aterm_spec::derive::streaming_search_model()`) — proven at build time by
//!   this crate's `build.rs` temporal gate, bound to the real engine in
//!   `tests/conformance_streaming.rs` (supersedes the never-committed hand
//!   `StreamingSearch.tla`)

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![deny(clippy::all)]

use std::borrow::Cow;

mod bitmap;
mod bloom;
mod budgeted;
mod bytesearch;
mod grapheme;
mod index;
mod iterators;
mod lifecycle_driver;
mod lifecycle_index;
mod literal;
pub mod streaming;
mod types;

pub use bloom::BloomFilter;
pub use budgeted::BudgetedSearch;
pub use grapheme::display_columns;
pub use index::{
    DEFAULT_MAX_CACHED_LINES, MAX_SEARCH_MATCHES, NarrowedSearch, SearchIndex, SearchOptionsError,
    max_cached_for_retained,
};
pub use lifecycle_driver::SearchLifecycleDriver;
pub use lifecycle_index::{
    AbsRowMatch, LifecycleSearchIndex, LifecycleSearchResults, SearchLifecycleEvent,
    U32PayloadResults, UpsertOutcome,
};
pub use types::{DirectedFind, SearchDirection, SearchMatch, SearchResults};

#[cfg(test)]
mod lifecycle_oracle_tests;
#[cfg(test)]
mod tests;

#[cfg(kani)]
mod proofs;

// ffi_kani_gaps removed (#5887): all three harnesses were null-guard proofs.

/// Terminal search that integrates with Grid and Scrollback.
///
/// This provides a unified interface for searching across:
/// - Current visible grid content
/// - Ring buffer scrollback
/// - Tiered scrollback (hot/warm/cold)
///
/// ## Staleness Detection
///
/// Every mutation to the search index (indexing, clearing, re-indexing) bumps
/// an internal generation counter. Consumers that cache search results should
/// snapshot the generation via [`generation()`](Self::generation) before
/// searching and compare it before using the results. A mismatch means the
/// index was mutated and the cached coordinates may be stale.
#[derive(Debug)]
pub struct TerminalSearch {
    /// Search index for all content.
    index: SearchIndex,
    /// Number of lines from scrollback that have been indexed.
    indexed_scrollback_lines: usize,
    /// Monotonically increasing generation counter.
    ///
    /// Bumped on every index mutation (line add, update, clear, invalidation).
    /// Consumers snapshot this before a search and compare before using results
    /// to detect stale match coordinates (#7271).
    generation: u64,
}

impl TerminalSearch {
    /// Create a new terminal search.
    #[must_use]
    pub fn new() -> Self {
        Self {
            index: SearchIndex::new(),
            indexed_scrollback_lines: 0,
            generation: 0,
        }
    }

    /// Create with expected capacity.
    #[must_use]
    pub fn with_capacity(expected_lines: usize) -> Self {
        // Branch-duplicated construction: SearchIndex::with_capacity clamps
        // its internal map capacity hints far below this bound and its bloom
        // filter saturates its size cap at BloomFilter::MAX_EFFECTIVE_CAPACITY,
        // so both arms construct the IDENTICAL index for any input in the
        // first arm. The Trust L0 gate's allocation recognizer needs the
        // comparison to directly dominate the allocating call, hence the
        // duplication.
        let index = if expected_lines > BloomFilter::MAX_EFFECTIVE_CAPACITY {
            SearchIndex::with_capacity(BloomFilter::MAX_EFFECTIVE_CAPACITY)
        } else {
            SearchIndex::with_capacity(expected_lines)
        };
        Self {
            index,
            indexed_scrollback_lines: 0,
            generation: 0,
        }
    }

    /// Create with expected capacity and an explicit cache cap.
    ///
    /// `max_cached_lines` bounds how many indexed lines are retained before the
    /// oldest are evicted. Use this when the scrollback window the GUI wants
    /// searchable differs from [`DEFAULT_MAX_CACHED_LINES`]. Eviction is
    /// observable via [`results_may_be_incomplete`](Self::results_may_be_incomplete).
    #[must_use]
    pub fn with_capacity_and_max(expected_lines: usize, max_cached_lines: usize) -> Self {
        Self {
            index: SearchIndex::with_capacity_and_max(expected_lines, max_cached_lines),
            indexed_scrollback_lines: 0,
            generation: 0,
        }
    }

    /// Set the maximum number of cached lines before eviction.
    ///
    /// Forwarded to the underlying [`SearchIndex`]. A value of 0 is clamped to
    /// 1. Does not bump the generation counter (no indexed content changes).
    pub fn set_max_cached_lines(&mut self, max: usize) {
        self.index.set_max_cached_lines(max);
    }

    /// The oldest line still retained in the index.
    ///
    /// See [`SearchIndex::lowest_retained_line`]. Matches below this line have
    /// been evicted; the searchable range is `[lowest_retained_line(),
    /// indexed_line_count())`.
    #[must_use]
    pub fn lowest_retained_line(&self) -> usize {
        self.index.lowest_retained_line()
    }

    /// Whether search results may be incomplete due to eviction.
    ///
    /// See [`SearchIndex::results_may_be_incomplete`]. When true, `cmd_search`
    /// should tell the AI results are truncated rather than exhaustive.
    #[must_use]
    pub fn results_may_be_incomplete(&self) -> bool {
        self.index.results_may_be_incomplete()
    }

    /// Get the current generation counter.
    ///
    /// Snapshot this value before a search operation. If the generation has
    /// changed by the time you use the results, the match coordinates may
    /// be stale and should be discarded or re-queried.
    #[must_use]
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bump the generation counter (internal helper).
    #[inline]
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Index a scrollback line.
    ///
    /// Call this when lines are pushed to scrollback.
    pub fn index_scrollback_line(&mut self, text: &str) {
        self.index.push_line(text);
        // Saturating: the count can never actually reach usize::MAX (each
        // increment requires indexing a real line), so this is identical to
        // `+= 1` on every reachable path while carrying the no-overflow proof
        // for the Trust L0 gate.
        self.indexed_scrollback_lines = self.indexed_scrollback_lines.saturating_add(1);
        self.bump_generation();
    }

    /// Index multiple scrollback lines.
    pub fn index_scrollback_lines(&mut self, lines: impl IntoIterator<Item = impl AsRef<str>>) {
        for line in lines {
            self.index_scrollback_line(line.as_ref());
        }
    }

    /// Re-index visible grid content.
    ///
    /// Call this to update the index with current grid content.
    /// Pass the visible content as an iterator of (line_index, text).
    pub fn index_visible_content(
        &mut self,
        base_line: usize,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        for (offset, line) in lines.into_iter().enumerate() {
            // Saturating: `base_line + offset` cannot overflow for real grids
            // (line numbers are bounded by scrollback limits), so this is
            // identical to `+` on every reachable path while carrying the
            // no-overflow proof for the Trust L0 gate.
            self.index.index_line_cow(
                base_line.saturating_add(offset),
                Cow::Borrowed(line.as_ref()),
            );
        }
        self.bump_generation();
    }

    /// Index owned lines at explicit absolute row numbers.
    ///
    /// Each `String` is moved into the retained index cache, avoiding the
    /// second allocation required by a borrowed line. The generation advances
    /// once for the whole batch, matching [`index_visible_content`](Self::index_visible_content).
    pub fn index_numbered_content_owned(
        &mut self,
        lines: impl IntoIterator<Item = (usize, String)>,
    ) {
        for (absolute_row, line) in lines {
            self.index.index_line_cow(absolute_row, Cow::Owned(line));
        }
        self.bump_generation();
    }

    /// State whether the bounded bulk snapshot that just finished omitted an
    /// older history prefix — `Some(oldest_indexed_row)` when it did (results
    /// report `incomplete` from exactly that row, without indexing rows only to
    /// evict them), `None` when the whole retained window is indexed and
    /// results are exhaustive again.
    ///
    /// See [`SearchIndex::set_history_prefix_eviction`] for why a bulk builder
    /// gets to clear the flag that the incremental append path latches.
    pub fn set_history_prefix_eviction(&mut self, lowest_retained_line: Option<usize>) {
        self.index.set_history_prefix_eviction(lowest_retained_line);
        self.bump_generation();
    }

    /// Advance a cached bulk index to a newer retained absolute-row boundary.
    pub fn retain_history_from(&mut self, first_retained_line: usize) {
        self.index.retain_history_from(first_retained_line);
        self.bump_generation();
    }

    /// Advance a cached index past rows the terminal no longer retains,
    /// WITHOUT the eviction-honesty bookkeeping — the complete-retention twin
    /// of [`retain_history_from`](Self::retain_history_from). See
    /// [`SearchIndex::drop_history_below`] for the contract (the refreshed
    /// index must stay observationally identical to a from-scratch rebuild
    /// over the surviving rows).
    pub fn drop_history_below(&mut self, first_retained_line: usize) {
        self.index.drop_history_below(first_retained_line);
        self.bump_generation();
    }

    /// Notify the search index that grid content has been invalidated.
    ///
    /// Call this when lines are deleted, scrollback is evicted, or the grid
    /// is cleared. This bumps the generation counter so that consumers holding
    /// stale `SearchMatch` coordinates can detect the invalidation.
    ///
    /// This does NOT remove entries from the underlying trigram index. If the
    /// invalidated lines need to be removed from the index, call [`clear()`]
    /// followed by re-indexing.
    pub fn invalidate(&mut self) {
        self.bump_generation();
    }

    /// Check if a query might have matches.
    #[must_use]
    pub fn might_contain(&self, query: &str) -> bool {
        self.index.might_contain(query)
    }

    /// Search for a query string.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        self.index.search_with_positions(query)
    }

    /// Search with options for case sensitivity and regex mode.
    ///
    /// When `case_sensitive` is true and `is_regex` is false, this uses the
    /// trigram-accelerated search path. Otherwise, all cached lines are scanned
    /// directly.
    pub fn search_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<Vec<SearchMatch>, SearchOptionsError> {
        self.index
            .search_with_positions_opts(query, case_sensitive, is_regex)
    }

    /// Search with options, returning matches bundled with the eviction signal.
    ///
    /// This is the entry point intended for the GUI's `cmd_search`: it returns
    /// absolute-row [`SearchMatch`]es plus an `incomplete` flag and the oldest
    /// searchable line, so truncated results can be reported to the AI honestly.
    /// See [`SearchIndex::search_results_opts`].
    pub fn search_results_opts(
        &self,
        query: &str,
        case_sensitive: bool,
        is_regex: bool,
    ) -> Result<SearchResults, SearchOptionsError> {
        self.index
            .search_results_opts(query, case_sensitive, is_regex)
    }

    /// Search with options while retaining the capped edge for `direction`.
    ///
    /// Results remain in ascending coordinate order. Forward search retains the
    /// oldest [`MAX_SEARCH_MATCHES`] while backward search retains the newest,
    /// allowing reverse incremental search to start at the actual newest hit.
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

    /// Cheap upper bound on the engine's own candidate count for a literal
    /// query — see [`SearchIndex::literal_candidate_bound`]. Narrowing policy
    /// layers compare a frame's length against this and skip the frame when
    /// the index would visit fewer lines.
    #[must_use]
    pub fn literal_candidate_bound(&self, query: &str, case_sensitive: bool) -> Option<u64> {
        self.index.literal_candidate_bound(query, case_sensitive)
    }

    /// One incremental-search narrowing step — see
    /// [`SearchIndex::search_literal_narrowed`] for the full contract
    /// (results-identity with the batch path, the occurrence-frame subset
    /// property, and the capped/backward fallback rules).
    #[must_use]
    pub fn search_literal_narrowed(
        &self,
        query: &str,
        case_sensitive: bool,
        prev_lines: Option<&[u32]>,
    ) -> NarrowedSearch {
        self.index
            .search_literal_narrowed(query, case_sensitive, prev_lines)
    }

    /// Search in the specified direction.
    pub fn search_ordered(&self, query: &str, direction: SearchDirection) -> Vec<SearchMatch> {
        self.index.search_ordered(query, direction)
    }

    /// Find the next match after the given position.
    ///
    /// This uses O(log n) range queries to skip lines before `after_line`,
    /// then iterates with early termination to find the first match.
    pub fn find_next(
        &self,
        query: &str,
        after_line: usize,
        after_col: usize,
    ) -> Option<SearchMatch> {
        // Use optimized range query starting from after_line
        self.index
            .search_from_line(query, after_line)
            .find(|m| m.line > after_line || (m.line == after_line && m.start_col > after_col))
    }

    /// Find the next match with case-sensitivity and regex options.
    ///
    /// Literal searches are lazy and stop at the first qualifying match. The
    /// default case-insensitive path uses allocation-free ASCII matching and
    /// range-bounded candidate iteration. Regex mode retains the batch regex
    /// implementation and can return [`SearchOptionsError::RegexNotEnabled`]
    /// when the crate feature is disabled.
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
    ///
    /// This uses O(log n) range queries to only search lines before `before_line`,
    /// then iterates with early termination to find the first match.
    pub fn find_prev(
        &self,
        query: &str,
        before_line: usize,
        before_col: usize,
    ) -> Option<SearchMatch> {
        // Include `before_line` itself by using an exclusive upper bound.
        // Saturate at `usize::MAX` to avoid overflow for sentinel/high-bound callers.
        let exclusive_upper = before_line.saturating_add(1);
        self.index
            .search_before_line(query, exclusive_upper)
            .find(|m| m.line < before_line || (m.line == before_line && m.start_col < before_col))
    }

    /// Find the previous match with case-sensitivity and regex options.
    ///
    /// Literal searches iterate candidates and lines in reverse order without
    /// materializing a candidate vector. Regex mode retains the batch path; see
    /// [`find_next_opts`](Self::find_next_opts).
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

    /// Find one match from an absolute anchor in `direction`.
    ///
    /// Unlike a batch search, this is not affected by [`MAX_SEARCH_MATCHES`].
    /// Literal modes use range-bounded candidate iterators and stop at the first
    /// qualifying match. `inclusive` controls whether a match exactly at the
    /// anchor qualifies; `wrap` retries from the opposite buffer edge when the
    /// directional suffix/prefix has no match.
    #[allow(
        clippy::too_many_arguments,
        reason = "public compatibility surface: each search-policy flag is independently meaningful"
    )]
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
    #[must_use]
    #[doc(hidden)]
    pub fn indexed_line_count(&self) -> usize {
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
        self.bump_generation();
    }

    /// Release the index's backing allocations (idle eviction primitive).
    ///
    /// Same observable reset as [`clear`](Self::clear) — the index is emptied
    /// and the generation bumped so any in-flight match coordinates are treated
    /// as stale — but the grown `HashMap`/`Vec` capacity is actually returned to
    /// the allocator instead of retained. An idle-eviction policy calls this to
    /// reclaim a dormant terminal's search footprint; the next indexing pass
    /// regrows the maps from empty. Use [`clear`](Self::clear) instead when the
    /// index will immediately be refilled and the peak capacity is worth keeping.
    pub fn release(&mut self) {
        self.index.release();
        self.indexed_scrollback_lines = 0;
        self.bump_generation();
    }
}

impl Default for TerminalSearch {
    fn default() -> Self {
        Self::new()
    }
}
