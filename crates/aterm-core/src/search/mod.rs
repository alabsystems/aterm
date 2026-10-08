// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Search surface for aterm-core; the logic lives in `aterm-search`.
//!
//! The `SearchContent` impls live beside their types (orphan rules): `Grid`'s
//! in `aterm-grid`, the `Scrollback*` adapters in `aterm-scrollback`.

pub use aterm_search::streaming;
pub use aterm_search::{
    BloomFilter, BudgetedSearch, DEFAULT_MAX_CACHED_LINES, MAX_SEARCH_MATCHES, NarrowedSearch,
    SearchDirection, SearchIndex, SearchMatch, SearchOptionsError, SearchResults, TerminalSearch,
    max_cached_for_retained,
};
