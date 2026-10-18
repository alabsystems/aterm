// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance harness: feed input, read the screen, assert, with the engine's
//! introspection API (`visible_content` / `row_text` / `cursor`) as the oracle
//! (ATERM_DESIGN §6.5).
//!
//! [`measure`] holds the `measure` check's instruments: the paint scanner, the
//! starvation account and the drivers of the release binary's matrices.

#[cfg(unix)]
pub mod measure;

use aterm_core::terminal::Terminal;

/// A terminal under test you feed bytes into and read the screen out of.
pub struct Screen {
    term: Terminal,
    pub rows: u16,
    pub cols: u16,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Screen {
            term: Terminal::new(rows, cols),
            rows,
            cols,
        }
    }

    /// Feed input bytes through the VT engine.
    pub fn feed(&mut self, input: &[u8]) -> &mut Self {
        self.term.process(input);
        self
    }

    /// Text of a visible row (row 0 = top), trailing blanks trimmed.
    pub fn row(&self, r: usize) -> String {
        self.term
            .row_text(r)
            .unwrap_or_default()
            .trim_end()
            .to_string()
    }

    /// Cursor position as (row, col), 0-based.
    pub fn cursor(&self) -> (u16, u16) {
        let c = self.term.cursor();
        (c.row, c.col)
    }

    /// The whole visible screen as one string (trailing blank lines trimmed).
    pub fn screen(&self) -> String {
        self.term.visible_content().trim_end().to_string()
    }

    /// The current SGR style as `(fg_packed, bg_packed, flags)`: what the next
    /// written cell gets. Pins the flags-only and colour-only SGR fast paths
    /// against the generic path.
    pub fn style_fingerprint(&self) -> (u32, u32, u16) {
        let st = self.term.style();
        (st.fg_packed(), st.bg_packed(), st.flags_bits())
    }

    /// Resolved attribute flags of the visible cell at `(r, c)` (0-based), as
    /// raw [`CellFlags`](aterm_core::grid::CellFlags) bits; `0` out of range.
    ///
    /// Resolves as `cell_attrs` in aterm-gui/src/control_query.rs does: the
    /// cell's inline flags.
    pub fn cell_flags_bits(&self, r: u16, c: u16) -> u16 {
        self.term
            .grid()
            .cell(r, c)
            .map_or(0, |cell| cell.flags().bits())
    }

    /// Hyperlink URL attached to the visible cell at `(r, c)` (0-based), if any.
    ///
    /// A URI shows here only if it passed the OSC 8 handler's scheme allowlist
    /// and length/control-char/BiDi guards and a glyph was printed while the
    /// link was open.
    pub fn hyperlink_at(&self, r: u16, c: u16) -> Option<String> {
        self.term.hyperlink_at(r, c).map(str::to_string)
    }

    /// Drain pending terminal responses (DSR/DA/DECRQSS/... replies); `None`
    /// when none is pending. `feed()` never drains, so replies concatenate
    /// until taken here.
    pub fn take_response(&mut self) -> Option<Vec<u8>> {
        self.term.take_response()
    }

    /// Drain pending responses as a (lossy UTF-8) string; empty if none.
    pub fn response_string(&mut self) -> String {
        self.take_response()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }

    /// Inline-image references placed on visible row `r` (0-based), as
    /// `(col, ImageRef)` pairs in column order.
    ///
    /// An inline image (OSC 1337 `File=` or a decoded sixel) lives in per-cell
    /// image extras, not glyph cells, so `row()`/`screen()` do not show it.
    pub fn images_row(&self, r: usize) -> Vec<(usize, aterm_core::grid::extra::ImageRef)> {
        self.term.images_row(r)
    }
}

/// Feed input to a default 24×80 screen and return it.
pub fn run(input: &[u8]) -> Screen {
    let mut s = Screen::new(24, 80);
    s.feed(input);
    s
}
