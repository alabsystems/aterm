// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grapheme cluster support — re-exports from the `aterm-grapheme` crate.
//!
//! The implementation lives in [`aterm_grapheme`]. This module provides
//! backward-compatible re-exports so existing `crate::grapheme::*`
//! paths continue to resolve.

// Re-export public API (cross-crate consumers)
pub use aterm_grapheme::{GraphemeInfo, byte_to_column, column_to_char_index, grapheme_width};

// Re-export crate-internal API
pub(crate) use aterm_grapheme::split_graphemes;

// Re-export test-only API (used by tests/proptest/scrollback.rs)
#[cfg(test)]
pub(crate) use aterm_grapheme::grapheme_display_width;
