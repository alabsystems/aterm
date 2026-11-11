// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nonpainting terminal projection for document readers.

use super::{RenderCell, Terminal};
use crate::render::{RenderInput, SelectionRowKey};

/// Refillable visible cells, cursor and selection, without paint payloads.
///
/// ```
/// use aterm_core::terminal::{Terminal, TextFrame};
/// let mut terminal = Terminal::new(2, 8);
/// let mut text = TextFrame::default();
/// terminal.text_frame_into(&mut text, 2, 8);
/// assert_eq!(text.cells().len(), 2);
/// ```
///
/// A text projection cannot be passed to a renderer expecting a complete frame.
///
/// ```compile_fail
/// use aterm_core::{render::RenderInput, terminal::TextFrame};
/// fn paint(_: &RenderInput) {}
/// paint(&TextFrame::default());
/// ```
#[derive(Default)]
pub struct TextFrame {
    pub(super) input: RenderInput,
}

impl TextFrame {
    /// Visible rows in the same order as a painted terminal frame.
    #[must_use]
    pub fn cells(&self) -> &[Vec<RenderCell>] {
        &self.input.cells
    }

    /// Distance from the live screen in rows.
    #[must_use]
    pub const fn display_offset(&self) -> i32 {
        self.input.display_offset
    }

    /// Cursor position when visible in this projection.
    #[must_use]
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.input
            .cursor_visible
            .then_some((self.input.cursor_row, self.input.cursor_col))
    }

    /// Whether any terminal text selection is present.
    #[must_use]
    pub const fn any_selection(&self) -> bool {
        self.input.any_selection()
    }

    /// Borrow the canonical selection spans for a visible row.
    pub fn selection_row_keys(&self, row: usize) -> impl Iterator<Item = SelectionRowKey> + '_ {
        self.input.selection_row_keys(row)
    }

    /// Overlay the focused grid's nonempty composition and expose its caret.
    pub fn overlay_ime_preedit(&mut self, preedit: &str, caret: Option<usize>, cjk: bool) {
        if !preedit.is_empty() {
            self.input.overlay_ime_preedit(preedit, caret, cjk);
            self.input.cursor_visible = true;
        }
    }
}

impl Terminal {
    /// Refill a nonpainting projection without consuming terminal damage.
    pub fn text_frame_into(&mut self, frame: &mut TextFrame, rows: usize, cols: usize) {
        self.cell_frame_fill::<false>(&mut frame.input, rows, cols, None);
    }
}

#[cfg(test)]
#[path = "text_frame_tests.rs"]
mod tests;
