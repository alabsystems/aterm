// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme position conversion: byte offsets and character indices to grid
//! columns and back.

use aterm_types::text_shaping::AmbiguousWidth;

use crate::grid::{grid_graphemes, grid_graphemes_with};

/// Convert a byte offset to a GRID column: the number of terminal cells the
/// clusters that start before `byte_offset` occupy.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a column in `0..=str_grid_columns(s)`, and
/// `byte_to_column(s, s.len()) == str_grid_columns(s)`.
///
/// Columns are the grid's (see [`grid_graphemes`]), not display width: every
/// caller measures text it extracted from the grid against grid columns (the
/// soft-wrap join pads each row out to the grid width; the double-click word
/// walk asks whether a clicked cell lies past the text). A Devanagari conjunct
/// is one cluster that LOOKS one cell wide and OCCUPIES three, so charging
/// display width put every later column two cells left of its text. Byte
/// offsets that fall inside a grapheme map to the column after that grapheme.
#[inline]
pub fn byte_to_column(s: &str, byte_offset: usize) -> usize {
    byte_to_column_with(s, byte_offset, AmbiguousWidth::Single)
}

/// [`byte_to_column`] under the terminal's `ambiguous_width` policy (see
/// [`grid_graphemes_with`]).
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
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a character index in `0..=s.chars().count()`.
///
/// Returns the character index at the start of the grapheme whose grid cells
/// (see [`grid_graphemes`], not display width) cover the given column. Columns
/// inside a multi-cell grapheme map to that grapheme's first character index.
/// A grapheme the grid folds into the cell before it (a zero-width format
/// character, the `ය` of Sinhala `ක්‍ය`) is drawn in that cell, which the
/// grapheme before it already names, so a column maps to it never.
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
