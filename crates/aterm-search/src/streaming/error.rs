// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Error types for streaming search.

/// Errors that can occur during streaming search.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchError {
    /// Empty pattern provided.
    EmptyPattern,
    /// Pattern exceeds maximum length.
    PatternTooLong,
    /// Invalid regex pattern.
    InvalidRegex(String),
    /// Operation not valid in current state.
    InvalidState,
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPattern => f.write_str("empty search pattern"),
            Self::PatternTooLong => f.write_str("pattern exceeds maximum length"),
            Self::InvalidRegex(e) => {
                f.write_str("invalid regex: ")?;
                f.write_str(e)
            }
            Self::InvalidState => f.write_str("operation not valid in current state"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Result type alias for search operations.
pub type SearchResult<T> = Result<T, SearchError>;
