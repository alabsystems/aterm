// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Streaming search with memory-bounded results.
//!
//! ## Spec of record: the DERIVED `StreamingSearch` machine
//!
//! `aterm_spec::derive::streaming_search_model()` — a drift-free `ty_model!`
//! twin registered in the global spec/ledger closure. It SUPERSEDES the
//! never-committed hand `StreamingSearch.tla` these docs used to reference.
//! Tier-0 prove-and-catch: `aterm-spec/tests/derived_streaming_search_ty.rs`
//! (+ the compile-time gate in this crate's `build.rs`); Tier-1 lockstep over a
//! bounded unit-effect alphabet against this real engine:
//! `aterm-search/tests/conformance_streaming.rs`. The per-method `#[refines]`
//! anchors in `engine/operations.rs` bind those model actions. Calls that add or
//! remove multiple matches atomically are deliberately outside that exact scalar
//! transition abstraction and remain covered by the local/Kani invariant suites.
//!
//! This module implements a streaming search system that:
//! - Searches through content incrementally (row by row)
//! - Bounds memory usage with configurable result limits
//! - Supports multiple filter modes: Literal, Regex, Fuzzy
//! - Provides navigation with optional wraparound
//! - Handles dynamic content changes (additions/invalidations)
//!
//! ## Safety Invariants
//!
//! | ID | Invariant | Discharged by |
//! |----|-----------|---------------|
//! | INV-SEARCH-1 | `CurrentIndexValid` | model Tier-0/Tier-1 + Kani |
//! | INV-SEARCH-2 | `ResultPositionsValid` | Kani only (not scalar-expressible) |
//! | INV-SEARCH-3 | `MemoryBounded` | model Tier-0/Tier-1 + Kani |
//! | INV-SEARCH-4 | `NoDuplicateResults` | Kani only (not scalar-expressible) |
//! | INV-SEARCH-5 | `ScanProgressConsistent` | model Tier-0/Tier-1 + Kani |
//! | INV-SEARCH-6 | `TotalMatchesConsistent` | model Tier-0/Tier-1 + Kani |
//!
//! INV-SEARCH-2/-4 quantify over per-result data (coordinates, duplicates), so
//! they remain Kani-owned bounded-local proofs (`proofs.rs`), ledger-joined via
//! `proof_anchor!` in `spec_proof_anchors.rs`.
//!
//! ## State Machine
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                                                             │
//! │  ┌──────┐  StartSearch   ┌───────────┐  ScanComplete       │
//! │  │ Idle │ ─────────────▶ │ Searching │ ──────────────┐     │
//! │  └──────┘                └───────────┘               │     │
//! │      ▲                        │                      ▼     │
//! │      │     Cancel             │ ScanComplete    ┌─────────┐│
//! │      ├────────────────────────┤                 │HasResult││
//! │      │                        │                 └─────────┘│
//! │      │     Cancel             ▼                      │     │
//! │      ├───────────────── ┌───────────┐               │     │
//! │      │                  │ NoResults │ ◀─────────────┘     │
//! │      │                  └───────────┘  (results empty)    │
//! │      │                        │                            │
//! │      └────────────────────────┘                            │
//! └─────────────────────────────────────────────────────────────┘
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

    /// Content provider with configurable wrapped-row flags for testing
    /// wrapped-line search coordinate remapping (#7572).
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

#[cfg(all(test, feature = "regex"))]
mod regex_tests;

#[cfg(kani)]
mod proofs;

#[cfg(kani)]
mod proofs_gaps;

// Kani half of the unified verifier ledger: `proof_anchor!` records joining the
// `#[cfg(kani)]` harnesses above to the derived StreamingSearch machine. The
// registrations are decoupled from the harnesses (which stock cargo strips) and
// gated ONLY by the `spec-anchors` feature / test builds — the §4 pattern.
#[cfg(any(test, feature = "spec-anchors"))]
mod spec_proof_anchors;
