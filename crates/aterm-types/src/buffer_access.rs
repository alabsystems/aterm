// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Read-only buffer access trait for unified navigation.
//!
//! `BufferAccess` provides a common interface for reading buffer content,
//! enabling navigation logic to operate on any text surface: terminal grid,
//! editor Rope buffer, or future navigable surfaces.

/// Read-only buffer access for navigation commands.
///
/// Both the terminal grid and the editor's Rope buffer implement this trait,
/// allowing navigation logic to operate on `dyn BufferAccess` without
/// coupling to concrete types.
///
/// Line coordinates use `i32` to support scrollback: negative values address
/// scrollback history, zero is the first visible line.
pub trait BufferAccess {
    /// Read the character at the given position.
    ///
    /// Returns `None` if the position is out of bounds. Returns `Some(' ')`
    /// for empty cells.
    fn char_at(&self, line: i32, col: u16) -> Option<char>;

    /// Length of the given line (last non-empty column + 1).
    ///
    /// Returns 0 for empty lines or out-of-bounds line numbers.
    fn line_len(&self, line: i32) -> u16;

    /// Total number of lines in the buffer (visible + scrollback/content).
    fn total_lines(&self) -> i32;

    /// Number of visible rows in the viewport.
    fn visible_rows(&self) -> u16;

    /// Number of columns in the viewport.
    fn cols(&self) -> u16;

    /// Extract the full text of a line.
    ///
    /// Returns `None` if the line number is out of bounds.
    fn line_text(&self, line: i32) -> Option<String>;

    /// Whether the cell at the given position is the first half of a wide character.
    ///
    /// Default implementation returns `false` (no wide characters). Override
    /// for terminal grids that track wide character state.
    fn is_wide(&self, _line: i32, _col: u16) -> bool {
        false
    }

    /// Current display offset (0 = live view, >0 = scrolled back).
    ///
    /// Default implementation returns 0 (no scrollback offset). Override
    /// for terminal grids that support scrollback navigation.
    fn display_offset(&self) -> i32 {
        0
    }

    /// Whether the given line is a soft-wrapped continuation of the previous line.
    ///
    /// Returns `true` when the line's content started on the previous row and
    /// wrapped due to reaching the column limit. Used by vi `^` (FirstOccupied)
    /// to walk backward to the logical line start.
    ///
    /// Default implementation returns `false` (no soft wrapping).
    fn is_line_wrapped(&self, _line: i32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal buffer implementation for testing.
    struct TestBuffer {
        lines: Vec<String>,
        visible: u16,
        width: u16,
    }

    impl BufferAccess for TestBuffer {
        fn char_at(&self, line: i32, col: u16) -> Option<char> {
            if line < 0 {
                return None;
            }
            let line = line as usize;
            self.lines
                .get(line)
                .and_then(|l| l.chars().nth(col as usize))
        }

        fn line_len(&self, line: i32) -> u16 {
            if line < 0 {
                return 0;
            }
            self.lines.get(line as usize).map_or(0, |l| l.len() as u16)
        }

        fn total_lines(&self) -> i32 {
            self.lines.len() as i32
        }

        fn visible_rows(&self) -> u16 {
            self.visible
        }

        fn cols(&self) -> u16 {
            self.width
        }

        fn line_text(&self, line: i32) -> Option<String> {
            if line < 0 {
                return None;
            }
            self.lines.get(line as usize).cloned()
        }
    }

    fn make_buffer() -> TestBuffer {
        TestBuffer {
            lines: vec!["Hello, world!".into(), "Second line".into(), "Third".into()],
            visible: 24,
            width: 80,
        }
    }

    #[test]
    fn test_char_at_valid() {
        let buf = make_buffer();
        assert_eq!(buf.char_at(0, 0), Some('H'));
        assert_eq!(buf.char_at(0, 7), Some('w'));
        assert_eq!(buf.char_at(1, 0), Some('S'));
    }

    #[test]
    fn test_char_at_out_of_bounds() {
        let buf = make_buffer();
        assert_eq!(buf.char_at(-1, 0), None);
        assert_eq!(buf.char_at(5, 0), None);
        assert_eq!(buf.char_at(0, 100), None);
    }

    #[test]
    fn test_line_len() {
        let buf = make_buffer();
        assert_eq!(buf.line_len(0), 13); // "Hello, world!"
        assert_eq!(buf.line_len(2), 5); // "Third"
        assert_eq!(buf.line_len(-1), 0);
        assert_eq!(buf.line_len(99), 0);
    }

    #[test]
    fn test_total_lines() {
        let buf = make_buffer();
        assert_eq!(buf.total_lines(), 3);
    }

    #[test]
    fn test_visible_rows_and_cols() {
        let buf = make_buffer();
        assert_eq!(buf.visible_rows(), 24);
        assert_eq!(buf.cols(), 80);
    }

    #[test]
    fn test_line_text() {
        let buf = make_buffer();
        assert_eq!(buf.line_text(0), Some("Hello, world!".into()));
        assert_eq!(buf.line_text(-1), None);
        assert_eq!(buf.line_text(99), None);
    }

    #[test]
    fn test_default_is_wide() {
        let buf = make_buffer();
        assert!(!buf.is_wide(0, 0));
    }

    #[test]
    fn test_default_display_offset() {
        let buf = make_buffer();
        assert_eq!(buf.display_offset(), 0);
    }
}
