// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance + integration test harness, founded on the engine's **read API**.
//!
//! The whole point of aterm is reliable screen reading; this makes testing easy:
//! a test is just **feed input → read the screen → assert**. The same machinery
//! the introspection API exposes (`visible_content` / `row_text` / `cursor`) is
//! the test oracle. When the frontend lands, the *same* feed→read→assert pattern
//! extends to `read_image` (pixels) and the rendered UI chrome (search box,
//! scrollbar) — see ATERM_DESIGN §6.5 (the self-proving harness).

use aterm_core::grid::cell_flags::CellFlags;
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

    /// Fingerprint of the current SGR style as `(fg_packed, bg_packed, flags)`.
    ///
    /// This is the exact (foreground, background, attribute-flag) state that the
    /// next written cell will be styled with, so two SGR sequences that should
    /// render identically must produce the same fingerprint. Used to pin the
    /// flags-only / colour-only SGR fast paths against the generic path.
    pub fn style_fingerprint(&self) -> (u32, u32, u16) {
        let st = self.term.style();
        (st.fg_packed(), st.bg_packed(), st.flags_bits())
    }

    /// Resolved attribute flags of the visible cell at `(r, c)` (0-based),
    /// as raw [`CellFlags`] bits.
    ///
    /// Read-only accessor for per-cell attribute assertions (DECCARA/DECFRA/
    /// DECSERA rect-op tests). Mirrors the resolution path used by the GUI
    /// control channel (`cell_attrs` in aterm-gui/src/control.rs): inline
    /// flags are returned directly; cells whose style is interned in the
    /// grid's `StyleTable` (`USES_STYLE_ID`) are rehydrated from the table —
    /// the same path `Terminal::render_row` uses for colors. Out-of-range or
    /// never-written cells yield `0`.
    pub fn cell_flags_bits(&self, r: u16, c: u16) -> u16 {
        let grid = self.term.grid();
        let Some(cell) = grid.cell(r, c) else {
            return 0;
        };
        if cell.uses_style_id() {
            let extra = cell.flags().difference(CellFlags::USES_STYLE_ID);
            grid.resolve_style_to_colors(cell.style_id(), extra)
                .2
                .bits()
        } else {
            cell.flags().bits()
        }
    }

    /// Hyperlink URL attached to the visible cell at `(r, c)` (0-based), if any.
    ///
    /// OSC 8 oracle: a URI is only observable here if it survived the
    /// scheme allowlist + length/control-char/BiDi guards in the OSC 8
    /// handler AND a glyph was printed while the link was open (the URI
    /// is attached per-cell at write time). Rejected URIs leave no trace.
    pub fn hyperlink_at(&self, r: u16, c: u16) -> Option<String> {
        self.term.hyperlink_at(r, c).map(str::to_string)
    }

    /// Drain pending terminal responses (DSR/DA/DECRQSS/... replies).
    ///
    /// The engine only *accumulates* response bytes in its internal buffer —
    /// `feed()`/`process()` never auto-drain it (only `Terminal::take_response`
    /// and a full reset clear it), so replies to several queries fed in one or
    /// many `feed()` calls concatenate until drained here. Returns `None` when
    /// no response is pending.
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
    /// The image observability oracle: an inline image (iTerm2 OSC 1337 `File=`
    /// or a decoded sixel) lives in per-cell image extras, NOT in glyph cells,
    /// so `row()`/`screen()` stay empty for it. This surfaces the same
    /// `ImageRef`s the renderer blits, exposing `ImageData.{format, bytes, cols,
    /// rows}` for placement + pixel assertions.
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
