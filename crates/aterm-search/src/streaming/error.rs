// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Error types for streaming search.

/// Errors that can occur during streaming search.
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Empty pattern provided.
    #[error("empty search pattern")]
    EmptyPattern,
    /// Pattern exceeds maximum length.
    #[error("pattern exceeds maximum length")]
    PatternTooLong,
    /// Invalid regex pattern.
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
    /// Operation not valid in current state.
    #[error("operation not valid in current state")]
    InvalidState,
}

/// Result type alias for search operations.
///
/// Represents `Ok(T)` on success or `Err(SearchError)` on failure.
pub type SearchResult<T> = Result<T, SearchError>;
