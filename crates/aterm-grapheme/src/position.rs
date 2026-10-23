// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme position conversion: byte offsets and character indices to grid columns and back.

use aterm_types::text_shaping::AmbiguousWidth;

use crate::grid::{grid_graphemes, grid_graphemes_with};

/// Convert a byte offset to a GRID column: the number of terminal cells the clusters that start
/// before `byte_offset` occupy.
#[inline]
pub fn byte_to_column(s: &str, byte_offset: usize) -> usize {
    byte_to_column_with(s, byte_offset, AmbiguousWidth::Single)
}

/// [`byte_to_column`] under the terminal's `ambiguous_width` policy (see [`grid_graphemes_with`]).
#[inline]
pub fn byte_to_column_with(s: &str, byte_offset: usize, ambiguous: AmbiguousWidth) -> usize {
    let mut column = 0usize;

    for g in grid_graphemes_with(s, ambiguous) {
        if g.byte_offset >= byte_offset {
            return column;
        }
        column = g.end_col;
    }

    column
}

/// Convert a GRID column to a character index.
#[inline]
pub fn column_to_char_index(s: &str, column: usize) -> usize {
    let mut char_index = 0usize;

    for g in grid_graphemes(s) {
        if column < g.end_col {
            return char_index;
        }
        char_index = char_index.saturating_add(g.text.chars().count());
    }

    char_index
}
