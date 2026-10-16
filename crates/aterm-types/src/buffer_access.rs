// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Read-only buffer access trait for navigation.

/// Read-only buffer access for navigation commands, so navigation logic (aterm-vi) runs on
/// `dyn BufferAccess`; the terminal grid implements it.
///
/// Line 0 is the top line of the live screen whatever
/// [`display_offset`](Self::display_offset) is; negative lines count back into scrollback
/// (-1 is the newest history line).
pub trait BufferAccess {
    /// The character at a position: `None` out of bounds, `Some(' ')` for an empty cell.
    fn char_at(&self, line: i32, col: u16) -> Option<char>;

    /// Length of the line (last non-empty column + 1); 0 for an empty or out-of-bounds line.
    fn line_len(&self, line: i32) -> u16;

    /// Total number of lines in the buffer (visible + scrollback/content).
    fn total_lines(&self) -> i32;

    /// Number of visible rows in the viewport.
    fn visible_rows(&self) -> u16;

    /// Number of columns in the viewport.
    fn cols(&self) -> u16;

    /// The full text of a line, or `None` if out of bounds.
    fn line_text(&self, line: i32) -> Option<String>;

    /// Whether the cell is the first half of a wide character. Default: `false`.
    fn is_wide(&self, _line: i32, _col: u16) -> bool {
        false
    }

    /// Wide-continuation flags for one line, one per column: `true` where the cell is the
    /// second half of a double-width character (which stores a literal `' '`, so a character
    /// classifier such as vi's word motions must skip it).
    ///
    /// Batched so a caller materializes a scrollback row once rather than once per cell through
    /// [`Self::is_wide`]. The default derives the flags from `is_wide` at that per-cell cost;
    /// implementations with row-level flags override it. `None` means no information: treat
    /// every cell as a non-continuation.
    fn line_wide_continuations(&self, line: i32) -> Option<Vec<bool>> {
        let cols = self.cols();
        if cols == 0 {
            return None;
        }
        Some(
            (0..cols)
                .map(|c| c > 0 && self.is_wide(line, c - 1))
                .collect(),
        )
    }

    /// Current display offset (0 = live view, >0 = scrolled back). Default: 0.
    fn display_offset(&self) -> i32 {
        0
    }

    /// Whether the line is a soft-wrapped continuation of the previous line (vi `^` walks
    /// back through these to the logical line start). Default: `false`.
    fn is_line_wrapped(&self, _line: i32) -> bool {
        false
    }
}
