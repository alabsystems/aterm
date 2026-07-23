// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Search surface for aterm-core.
//!
//! Pure search logic lives in `aterm-search`.
//!
//! `Scrollback*` adapters live in `aterm-scrollback`, where those types are
//! local and can satisfy Rust's orphan rules.

pub use aterm_search::streaming;
pub use aterm_search::{
    BloomFilter, BudgetedSearch, DEFAULT_MAX_CACHED_LINES, MAX_SEARCH_MATCHES, SearchDirection,
    SearchIndex, SearchMatch, SearchOptionsError, SearchResults, TerminalSearch,
    max_cached_for_retained,
};

// SearchContent impl for Grid moved to aterm-grid (#6554, orphan rules).
