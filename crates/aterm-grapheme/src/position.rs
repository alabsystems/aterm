// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme position conversion, cell assignment, and segmentation utilities.

#[cfg(any(test, kani))]
use crate::types::Grapheme;
#[cfg(any(test, kani))]
use crate::width::grapheme_width;
use crate::width::split_graphemes;

/// Find the grapheme cluster that contains a given byte offset.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: `byte_offset >= s.len()` implies `None`; `Some(g)` implies `byte_offset` is inside `g`'s byte range.
///
/// Returns `None` if the offset is out of bounds.
#[cfg(any(test, kani))]
pub fn grapheme_at_byte(s: &str, byte_offset: usize) -> Option<Grapheme<'_>> {
    if byte_offset >= s.len() {
        return None;
    }

    for g in split_graphemes(s) {
        let end = g.byte_offset + g.text.len();
        if byte_offset >= g.byte_offset && byte_offset < end {
            return Some(g);
        }
    }

    None
}

/// Find the grapheme cluster at a given display column.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: `Some(g)` implies `column` lies in that grapheme's display-column span.
///
/// Returns `None` if the column is beyond the string's display width.
#[cfg(any(test, kani))]
pub fn grapheme_at_column(s: &str, column: usize) -> Option<Grapheme<'_>> {
    let mut current_col = 0;

    for g in split_graphemes(s) {
        if column >= current_col && column < current_col + g.width.max(1) {
            return Some(g);
        }
        current_col += g.width;
    }

    None
}

/// Convert a byte offset to a display column.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a column in `0..=grapheme_width(s).display_width`.
///
/// Returns the column position after accounting for all graphemes that
/// start before the byte offset. Byte offsets that fall inside a grapheme
/// map to the column after that grapheme.
pub fn byte_to_column(s: &str, byte_offset: usize) -> usize {
    let mut column = 0;

    for g in split_graphemes(s) {
        if g.byte_offset >= byte_offset {
            return column;
        }
        column += g.width;
    }

    column
}

/// Convert a display column to a byte offset.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a byte offset in `0..=s.len()` at a grapheme boundary (or `s.len()` past the end).
///
/// Returns the byte offset at the start of the grapheme at the given column.
#[cfg(any(test, kani))]
pub fn column_to_byte(s: &str, column: usize) -> usize {
    let mut current_col = 0;

    for g in split_graphemes(s) {
        if column < current_col + g.width.max(1) {
            return g.byte_offset;
        }
        current_col += g.width;
    }

    s.len()
}

/// Convert a display column to a character index.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a character index in `0..=s.chars().count()`.
///
/// Returns the character index at the start of the grapheme that covers
/// the given column. Columns inside a wide grapheme map to that grapheme's
/// first character index. Zero-width graphemes (standalone combining marks,
/// ZWJ, ZWNJ) are treated as occupying 1 column for mapping purposes,
/// consistent with `column_to_byte` and `grapheme_at_column`.
pub fn column_to_char_index(s: &str, column: usize) -> usize {
    let mut current_col = 0usize;
    let mut char_index = 0usize;

    for g in split_graphemes(s) {
        // Zero-width graphemes (standalone combining marks, ZWJ, ZWNJ) are
        // treated as occupying at least 1 column for column-to-position
        // mapping, matching the convention in column_to_byte, assign_cells,
        // and grapheme_at_column.
        let effective_width = g.width.max(1);
        let next_col = current_col + effective_width;
        if column < next_col {
            return char_index;
        }
        current_col = next_col;
        char_index += g.text.chars().count();
    }

    char_index
}

/// Truncate a string to fit within a given display width.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a prefix of `s` whose display width is at most `max_width` and does not split graphemes.
#[cfg(any(test, kani))]
pub fn truncate_to_width(s: &str, max_width: usize) -> &str {
    let mut width = 0;
    let mut end_byte = 0;

    for g in split_graphemes(s) {
        if width + g.width > max_width {
            break;
        }
        width += g.width;
        end_byte = g.byte_offset + g.text.len();
    }

    &s[..end_byte]
}

/// Pad a string to a given display width.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: returned string has display width `<= width` and begins with `truncate_to_width(s, width)`.
#[cfg(any(test, kani))]
pub fn pad_to_width(s: &str, width: usize) -> String {
    let info = grapheme_width(s);

    if info.display_width >= width {
        truncate_to_width(s, width).to_string()
    } else {
        let padding = width - info.display_width;
        let mut result = s.to_string();
        result.push_str(&" ".repeat(padding));
        result
    }
}

/// Check if a string is entirely ASCII (fast path for terminals).
///
/// ASCII-only text can use simpler width calculation.
#[cfg(any(test, kani))]
#[inline]
pub fn is_ascii_only(s: &str) -> bool {
    s.bytes().all(|b| b < 128)
}

/// Fast width calculation for ASCII-only strings.
///
/// O(n) in string length: counts printable ASCII bytes (0x20..0x7F).
/// Each ASCII character is 1 cell wide (control characters are 0).
#[cfg(any(test, kani))]
#[inline]
pub fn ascii_width(s: &str) -> usize {
    s.bytes().filter(|&b| (0x20..0x7F).contains(&b)).count()
}

/// Terminal-aware grapheme segmenter for processing input.
///
/// This struct provides stateful grapheme processing suitable for
/// terminal input handling, tracking position information for
/// cursor management.
#[cfg(any(test, kani))]
#[derive(Debug, Clone)]
pub struct GraphemeSegmenter {
    /// Current column position.
    column: usize,
    /// Current grapheme index.
    index: usize,
}

#[cfg(any(test, kani))]
impl Default for GraphemeSegmenter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, kani))]
impl GraphemeSegmenter {
    /// Create a new segmenter starting at column 0.
    #[inline]
    pub fn new() -> Self {
        Self {
            column: 0,
            index: 0,
        }
    }

    /// Get the current column position.
    #[inline]
    pub fn column(&self) -> usize {
        self.column
    }

    /// Get the current grapheme index.
    #[inline]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Process a string and return position after all graphemes.
    pub fn process_string(&mut self, s: &str) -> crate::types::GraphemeInfo {
        let info = grapheme_width(s);
        self.column += info.display_width;
        self.index += info.grapheme_count;
        info
    }
}

/// Cell assignment for a grapheme cluster.
///
/// When rendering graphemes to terminal cells, a grapheme may span
/// 1 or 2 cells. This struct describes the cell assignment.
#[cfg(any(test, kani))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphemeCells {
    /// First cell column.
    pub start_col: usize,
    /// Number of cells (1 or 2).
    pub cell_count: usize,
    /// Whether this is a wide character (spans 2 cells).
    pub is_wide: bool,
}

#[cfg(any(test, kani))]
impl GraphemeCells {
    /// Get the ending column (exclusive).
    #[inline]
    pub fn end_col(&self) -> usize {
        self.start_col + self.cell_count
    }

    /// Check if a column is within this grapheme's cells.
    #[inline]
    pub fn contains_col(&self, col: usize) -> bool {
        col >= self.start_col && col < self.end_col()
    }
}

/// Assign cells to graphemes in a string.
///
/// Returns an iterator of (grapheme, cells) pairs showing how each
/// grapheme maps to terminal cells.
#[cfg(any(test, kani))]
pub fn assign_cells(
    s: &str,
    start_col: usize,
) -> impl Iterator<Item = (Grapheme<'_>, GraphemeCells)> {
    let mut col = start_col;

    split_graphemes(s).map(move |g| {
        let cells = GraphemeCells {
            start_col: col,
            cell_count: g.width.max(1),
            is_wide: g.width == 2,
        };
        col += cells.cell_count;
        (g, cells)
    })
}
