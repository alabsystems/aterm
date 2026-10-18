// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Search surface for aterm-core; the logic lives in `aterm-search`.

pub use aterm_search::{
    BloomFilter, BudgetedSearch, DEFAULT_MAX_CACHED_LINES, LineCells, MAX_SEARCH_MATCHES,
    NarrowedSearch, SearchDirection, SearchIndex, SearchMatch, SearchOptionsError, SearchResults,
    TerminalSearch, max_cached_for_retained,
};
