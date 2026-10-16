// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Streaming search with memory-bounded results.
//!
//! Scans content row by row (literal, regex or fuzzy), stores at most `max_results`
//! matches, navigates them with optional wraparound, and follows content that is
//! added, reflowed or invalidated.
//!
//! Spec of record: the derived `StreamingSearch` machine
//! (`aterm_spec::derive::streaming_search_model()`), proven by aterm-spec's
//! `tests/derived_streaming_search_ty.rs` and run in lockstep against this engine by
//! `tests/conformance_streaming.rs` through the `#[refines]` anchors in
//! `engine/operations.rs`. Calls that add or remove several matches at once fall
//! outside that scalar model; `verify_all_invariants`, which the streaming tests
//! assert after every step, covers them.
//!
//! | ID | Invariant | Checked by |
//! |----|-----------|------------|
//! | INV-SEARCH-1 | `CurrentIndexValid` | model + `verify_all_invariants` |
//! | INV-SEARCH-2 | `ResultPositionsValid` | `verify_all_invariants` |
//! | INV-SEARCH-3 | `MemoryBounded` | model + `verify_all_invariants` |
//! | INV-SEARCH-4 | `NoDuplicateResults` | `verify_all_invariants` |
//! | INV-SEARCH-5 | `ScanProgressConsistent` | model + `verify_all_invariants` |
//! | INV-SEARCH-6 | `TotalMatchesConsistent` | model + `verify_all_invariants` |
//!
//! INV-SEARCH-2 and -4 quantify over per-result data, which no scalar model carries.
//!
//! ```text
//! any        ──start──────────────────────▶ Searching
//! Searching  ──last row scanned───────────▶ HasResults | NoResults
//! NoResults  ──content_added (a match)────▶ HasResults
//! HasResults ──content_invalidated (none)─▶ NoResults
//! any active ──content_reflowed───────────▶ Searching
//! any active ──cancel─────────────────────▶ Idle
//! ```

mod content;
mod engine;
mod error;
mod types;

pub use content::SearchContent;
pub use engine::StreamingSearch;
pub use error::{SearchError, SearchResult};
pub use types::{FilterMode, SearchState, StreamingMatch, StreamingSearchConfig};

#[cfg(test)]
mod test_content {
    use super::SearchContent;

    /// Simple content provider shared by streaming search tests.
    pub(super) struct TestContent {
        lines: Vec<String>,
    }

    impl TestContent {
        pub(super) fn new(lines: Vec<&str>) -> Self {
            Self {
                lines: lines.into_iter().map(String::from).collect(),
            }
        }
    }

    impl SearchContent for TestContent {
        fn row_count(&self) -> usize {
            self.lines.len()
        }

        fn get_row_text(&mut self, row: usize) -> Option<String> {
            self.lines.get(row).cloned()
        }
    }

    /// Content provider with per-row soft-wrap flags.
    pub(super) struct WrappedTestContent {
        lines: Vec<String>,
        /// Which rows are continuations of the previous row (soft wrap).
        wrapped: Vec<bool>,
    }

    impl WrappedTestContent {
        /// Create content where `wrapped[i]` indicates row `i` is a continuation.
        pub(super) fn new(lines: Vec<&str>, wrapped: Vec<bool>) -> Self {
            assert_eq!(lines.len(), wrapped.len());
            Self {
                lines: lines.into_iter().map(String::from).collect(),
                wrapped,
            }
        }
    }

    impl SearchContent for WrappedTestContent {
        fn row_count(&self) -> usize {
            self.lines.len()
        }

        fn get_row_text(&mut self, row: usize) -> Option<String> {
            self.lines.get(row).cloned()
        }

        fn is_row_wrapped(&self, row: usize) -> bool {
            self.wrapped.get(row).copied().unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod regex_tests;
