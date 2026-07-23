// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Content provider trait for streaming search.
//!
//! Defines the abstraction that content sources (grids, scrollback storage)
//! implement so that `aterm-search` can scan them without depending on their
//! concrete types. Lives in `aterm-types` so that both `aterm-search` (trait
//! consumer) and `aterm-scrollback` (trait implementor) can use it without
//! creating a coupling inversion (#5759).

/// Content provider trait for streaming search.
///
/// Implement this to allow streaming search over different content sources
/// (terminal grids, scrollback storage, etc.).
///
/// Note: `get_row_text` takes `&mut self` because disk-backed scrollback
/// uses an LRU cache that requires mutable access.
pub trait SearchContent {
    /// Get the total number of rows.
    fn row_count(&self) -> usize;

    /// Get the text content of a specific row.
    fn get_row_text(&mut self, row: usize) -> Option<String>;

    /// Check if a row is a continuation of the previous row (soft wrap).
    ///
    /// When a long line wraps to multiple grid rows, all continuation rows
    /// return `true`. The first row of a logical line returns `false`.
    /// This enables search to join consecutive wrapped rows into a single
    /// logical line so that queries spanning wrap boundaries can match (#7471).
    ///
    /// Default: `false` (no wrapping) for backward compatibility.
    fn is_row_wrapped(&self, _row: usize) -> bool {
        false
    }
}
