// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grapheme cluster support: re-exports from [`aterm_grapheme`] as
//! `crate::grapheme::*`.

pub use aterm_grapheme::{
    GraphemeInfo, byte_to_column, byte_to_column_with, column_to_char_index, grapheme_width,
};

// Used by test_support/generated/grapheme.rs.
#[cfg(test)]
pub(crate) use aterm_grapheme::{grapheme_display_width, split_graphemes};
