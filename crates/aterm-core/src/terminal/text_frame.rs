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
mod tests {
    use super::*;

    #[test]
    fn text_projection_omits_paint_streams() {
        let mut term = Terminal::new(4, 16);
        for row in 0..12 {
            term.process(format!("past {row}\r\n").as_bytes());
        }
        term.process(b"incoming");
        term.process("\x1b[1;1Hשלום e\u{301}".as_bytes());
        term.scroll_display(1);
        let painted = term.cell_frame(4, 16);
        assert_eq!(painted.display_offset, 1);
        assert!(
            painted.apron_row.present,
            "CONTROL: scrolled paint has an incoming row"
        );
        assert!(
            !painted.line_sizes.is_empty(),
            "CONTROL: paint has line metadata"
        );
        let mut text = TextFrame::default();
        term.text_frame_into(&mut text, 4, 16);
        assert!(text.input.line_sizes.is_empty());
        assert!(text.input.images.is_empty());
        assert!(text.input.clusters.is_empty());
        assert!(text.input.combining.is_empty());
        assert!(text.input.row_rev.is_empty());
        assert!(!text.input.apron_row.present);
    }

    #[test]
    fn text_projection_matches_paint_across_refills_without_consuming_damage() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut rich = Terminal::new(6, 24);
        rich.process("\x1b[31;47mשלום (אב) e\u{301} 👩‍💻\r\n中 ♥︎ ❤️\r\n\x1b#6double".as_bytes());
        rich.text_selection_mut()
            .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        rich.text_selection_mut()
            .update_selection(1, 8, SelectionSide::Right);
        rich.text_selection_mut().complete_selection();
        let mut plain = Terminal::new(6, 24);
        plain.process(b"\x1b[?25lplain");
        let mut history = Terminal::new(6, 24);
        for _ in 0..12 {
            history.process(b"past screen\r\n");
        }
        history.process(b"incoming");
        history.scroll_display(1);
        let mut alternate = Terminal::new(6, 24);
        alternate.process(b"\x1b[?1049halternate\r\n\x1b#6wide");
        let mut text = TextFrame::default();
        for _ in 0..3 {
            for term in [&mut rich, &mut plain, &mut history, &mut alternate] {
                for (rows, cols) in [(6, 24), (3, 8), (8, 30)] {
                    let damaged: Vec<_> = term.grid().damage().damaged_rows(term.rows()).collect();
                    let full = term.grid().damage().is_full();
                    let mut paint = term.cell_frame(rows, cols);
                    term.text_frame_into(&mut text, rows, cols);
                    assert_eq!(text.cells(), paint.cells);
                    assert_eq!(
                        text.cursor(),
                        paint
                            .cursor_visible
                            .then_some((paint.cursor_row, paint.cursor_col))
                    );
                    assert_eq!(text.display_offset(), paint.display_offset);
                    assert_eq!(text.any_selection(), paint.any_selection());
                    for row in 0..rows {
                        assert!(
                            text.selection_row_keys(row)
                                .eq(paint.selection_row_keys(row))
                        );
                    }
                    assert_eq!(
                        damaged,
                        term.grid()
                            .damage()
                            .damaged_rows(term.rows())
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(full, term.grid().damage().is_full());
                    text.overlay_ime_preedit("仮e\u{301}", Some(3), false);
                    paint.overlay_ime_preedit("仮e\u{301}", Some(3), false);
                    paint.cursor_visible = true;
                    assert_eq!(text.cells(), paint.cells);
                    assert_eq!(text.cursor(), Some((paint.cursor_row, paint.cursor_col)));
                }
            }
        }
    }
}
