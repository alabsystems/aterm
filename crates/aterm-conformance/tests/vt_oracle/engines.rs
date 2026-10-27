// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared screen-state projection for `aterm` and `astream-term` oracle comparison.

use astream_term::Record;
use astream_term::screen::{Color, Folder, attr};
use aterm_core::grid::{CellFlags, Grid};
use aterm_core::terminal::Terminal;
use std::fmt::Write as _;

/// A colour both engines can name: the terminal default, a palette index or a 24-bit colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
    /// aterm marks the colour 24-bit but its colour ring holds no value for the cell.
    RgbLost,
}

/// Which part of a glyph a cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// All of a narrow glyph, or a blank.
    Narrow,
    /// The first column of a wide glyph, which holds it.
    Lead,
    /// The second column of a wide glyph.
    Spacer,
}

/// One cell's rendition in the shared vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    pub fg: Paint,
    pub bg: Paint,
    pub bold: bool,
    pub underline: bool,
    pub reverse: bool,
    pub italic: bool,
    /// Blinking, slow or rapid alike.
    pub blink: bool,
    /// Crossed out.
    pub strike: bool,
    /// Concealed (aterm's `HIDDEN`).
    pub conceal: bool,
    /// Which part of a glyph the cell holds.
    pub half: Half,
    /// aterm-only attribute bits; always 0 for astream-term.
    pub extra: u16,
}

impl Ink {
    /// The default pen: default colours, no attribute.
    pub const PLAIN: Ink = Ink {
        fg: Paint::Default,
        bg: Paint::Default,
        bold: false,
        underline: false,
        reverse: false,
        italic: false,
        blink: false,
        strike: false,
        conceal: false,
        half: Half::Narrow,
        extra: 0,
    };

    /// The default pen in palette foreground `n`.
    pub const fn fg(n: u8) -> Ink {
        Ink {
            fg: Paint::Indexed(n),
            ..Ink::PLAIN
        }
    }

    /// This ink, bold.
    pub const fn bold(self) -> Ink {
        Ink { bold: true, ..self }
    }
}

/// One cell: its character (a blank is `' '` on both sides) and rendition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellView {
    pub ch: char,
    pub ink: Ink,
}

/// Everything the oracle compares, read off one engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snap {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<CellView>,
    /// `(row, col)`, 0-based; `col` is one past the cursor's column while a
    /// wrap is pending (`cols` at the screen's edge, one past a right
    /// margin under DECSLRM).
    pub cursor: (u16, u16),
    /// A wrap is pending, which tells a cursor one past a right margin from
    /// one really standing on the column after it.
    pub wrap: bool,
    pub cursor_visible: bool,
    pub alt_screen: bool,
}

impl Snap {
    /// Row `r`'s text, trailing blanks trimmed: a wide glyph once, its spacer adding nothing.
    pub fn row_text(&self, r: u16) -> String {
        let start = usize::from(r) * usize::from(self.cols);
        let line: String = self.cells[start..start + usize::from(self.cols)]
            .iter()
            .filter(|c| c.ink.half != Half::Spacer)
            .map(|c| c.ch)
            .collect();
        line.trim_end().to_string()
    }

    /// The screen's text, one `|`-fenced line per row, for a failure message.
    pub fn text_block(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            let _ = writeln!(out, "    |{}|", self.row_text(r));
        }
        out
    }

    /// The first way `other` differs from `self`, in words; `None` when equal.
    pub fn first_difference(&self, other: &Snap) -> Option<String> {
        if (self.cols, self.rows) != (other.cols, other.rows) {
            return Some(format!(
                "dims {}x{} vs {}x{}",
                self.cols, self.rows, other.cols, other.rows
            ));
        }
        for (i, (a, b)) in self.cells.iter().zip(&other.cells).enumerate() {
            if a != b {
                let (r, c) = (i / usize::from(self.cols), i % usize::from(self.cols));
                return Some(format!("cell ({r},{c}): {a:?} vs {b:?}"));
            }
        }
        if self.cursor != other.cursor {
            return Some(format!("cursor {:?} vs {:?}", self.cursor, other.cursor));
        }
        if self.wrap != other.wrap {
            return Some(format!("wrap pending {} vs {}", self.wrap, other.wrap));
        }
        if self.cursor_visible != other.cursor_visible {
            return Some(format!(
                "cursor visible {} vs {}",
                self.cursor_visible, other.cursor_visible
            ));
        }
        if self.alt_screen != other.alt_screen {
            return Some(format!(
                "alternate screen {} vs {}",
                self.alt_screen, other.alt_screen
            ));
        }
        None
    }
}

/// The fold's colour: its 24-bit override when it has one, else its palette or default colour.
fn paint_of(c: Color, rgb: Option<(u8, u8, u8)>) -> Paint {
    match (rgb, c) {
        (Some((r, g, b)), _) => Paint::Rgb(r, g, b),
        (None, Color::Default) => Paint::Default,
        (None, Color::Indexed(n)) => Paint::Indexed(n),
    }
}

/// astream-term's fold, advanced chunk by chunk.
pub struct Astream {
    folder: Folder,
}

impl Astream {
    pub fn new(cols: u16, rows: u16) -> Self {
        Astream {
            folder: Folder::new(cols, rows),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.folder.apply(&Record::Out(bytes.to_vec()));
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.folder.apply(&Record::Resize { cols, rows });
    }

    pub fn snap(&self) -> Snap {
        let s = self.folder.screen();
        let (cols, rows) = s.dims();
        let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
        for r in 0..rows {
            for c in 0..cols {
                let cell = s.cell(r, c).copied().unwrap_or(astream_term::Cell::BLANK);
                cells.push(CellView {
                    ch: cell.ch,
                    ink: Ink {
                        fg: paint_of(cell.fg, cell.fg_rgb),
                        bg: paint_of(cell.bg, cell.bg_rgb),
                        bold: cell.attrs & attr::BOLD != 0,
                        underline: cell.attrs & attr::UNDERLINE != 0,
                        reverse: cell.attrs & attr::REVERSE != 0,
                        italic: cell.attrs & attr::ITALIC != 0,
                        blink: cell.attrs & attr::BLINK != 0,
                        strike: cell.attrs & attr::STRIKE != 0,
                        conceal: cell.attrs & attr::CONCEAL != 0,
                        half: match cell.width {
                            2 => Half::Lead,
                            0 => Half::Spacer,
                            // 1; the fold holds every width to 0..=2 (`cell_width_is_valid`).
                            _ => Half::Narrow,
                        },
                        extra: u16::from(cell.attrs & !ASTREAM_MODELLED),
                    },
                });
            }
        }
        Snap {
            cols,
            rows,
            cells,
            cursor: s.cursor(),
            wrap: s.wrap_pending(),
            cursor_visible: s.cursor_visible(),
            alt_screen: s.is_altscreen(),
        }
    }
}

/// The fold's attribute bits [`Ink`] names; any other (FAINT) lands in `extra`.
const ASTREAM_MODELLED: u8 = attr::BOLD
    | attr::UNDERLINE
    | attr::REVERSE
    | attr::ITALIC
    | attr::BLINK
    | attr::STRIKE
    | attr::CONCEAL;

const UNDERLINE_BITS: u16 =
    CellFlags::UNDERLINE.0 | CellFlags::DOUBLE_UNDERLINE.0 | CellFlags::CURLY_UNDERLINE.0;
const MODELLED_BITS: u16 = CellFlags::BOLD.0
    | CellFlags::INVERSE.0
    | CellFlags::ITALIC.0
    | CellFlags::BLINK.0
    | CellFlags::STRIKETHROUGH.0
    | CellFlags::HIDDEN.0
    | UNDERLINE_BITS;

/// The production engine, advanced chunk by chunk.
pub struct Aterm {
    term: Terminal,
}

fn aterm_cell(grid: &Grid, r: u16, c: u16) -> CellView {
    let Some(cell) = grid.cell(r, c) else {
        return CellView {
            ch: ' ',
            ink: Ink::PLAIN,
        };
    };
    let colors = cell.colors();
    let fg = if colors.fg_is_default() {
        Paint::Default
    } else if colors.fg_is_indexed() {
        Paint::Indexed(colors.fg_index())
    } else {
        grid.fg_rgb_at(r, c)
            .map_or(Paint::RgbLost, |[red, green, blue]| {
                Paint::Rgb(red, green, blue)
            })
    };
    let bg = if colors.bg_is_default() {
        Paint::Default
    } else if colors.bg_is_indexed() {
        Paint::Indexed(colors.bg_index())
    } else {
        grid.bg_rgb_at(r, c)
            .map_or(Paint::RgbLost, |[red, green, blue]| {
                Paint::Rgb(red, green, blue)
            })
    };
    let flags = cell.flags().0;
    // Bit 10 is a spacer's role bit (WIDE_CONTINUATION) only on a spacer; on any other cell it
    // is PROTECTED, which stays in `extra`. COMPLEX (bit 15) says only that the cell's text lives in
    // its extras, as an emoji's or a cluster's does: `resolved_char` reads it, so it is no rendition.
    let half = if grid.is_wide_continuation_at(r, c) {
        Half::Spacer
    } else if flags & CellFlags::WIDE.0 != 0 {
        Half::Lead
    } else {
        Half::Narrow
    };
    let role = match half {
        Half::Spacer => CellFlags::WIDE_CONTINUATION.0,
        Half::Lead | Half::Narrow => CellFlags::WIDE.0,
    };
    let ch = match grid.resolved_char(r, c).unwrap_or(' ') {
        '\0' => ' ',
        other => other,
    };
    CellView {
        ch,
        ink: Ink {
            fg,
            bg,
            bold: flags & CellFlags::BOLD.0 != 0,
            underline: flags & UNDERLINE_BITS != 0,
            reverse: flags & CellFlags::INVERSE.0 != 0,
            italic: flags & CellFlags::ITALIC.0 != 0,
            blink: flags & CellFlags::BLINK.0 != 0,
            strike: flags & CellFlags::STRIKETHROUGH.0 != 0,
            conceal: flags & CellFlags::HIDDEN.0 != 0,
            half,
            extra: flags & !(MODELLED_BITS | role | CellFlags::COMPLEX.0),
        },
    }
}

impl Aterm {
    pub fn new(cols: u16, rows: u16) -> Self {
        Aterm {
            term: Terminal::new(rows, cols),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(rows, cols);
    }

    /// Whether a resize to `cols` x `rows` would leave the main screen other than with each row
    /// clipped or extended where it is (a cell, a row's continuation mark or the history's
    /// length changed), read off a fork of this engine. aterm rewraps the main screen (it merges
    /// continued rows, splits long ones, pushes rows into history, opens a continuation row for
    /// the cursor's cell past a row's end or for a saved cursor past a full row), where the
    /// fold, which has no reflow or history, clips every row in place: such a resize is one the
    /// fold cannot judge. The alternate screen resizes without reflow, so it never decides this.
    /// An engine that cannot fork (mid-sequence) answers `true`.
    pub fn reflow_moves_cells(&self, cols: u16, rows: u16) -> bool {
        let before = self.term.main_grid();
        // A width change rewraps continued rows as one line, whose blanks can move with the
        // cursor where no cell shows it.
        if cols != before.cols()
            && (0..before.rows()).any(|r| before.row(r).is_some_and(|row| row.is_wrapped()))
        {
            return true;
        }
        let Some(mut fork) = self.term.replay_fork() else {
            return true;
        };
        fork.resize(rows, cols);
        let after = fork.main_grid();
        if after.scrollback_lines() != before.scrollback_lines()
            || (0..rows.min(before.rows())).any(|r| {
                before.row(r).map(|row| row.is_wrapped())
                    != after.row(r).map(|row| row.is_wrapped())
            })
        {
            return true;
        }
        let blank = CellView {
            ch: ' ',
            ink: Ink::PLAIN,
        };
        (0..rows).any(|r| {
            (0..cols).any(|c| {
                let clipped = if r < before.rows() && c < before.cols() {
                    aterm_cell(before, r, c)
                } else {
                    blank
                };
                aterm_cell(after, r, c) != clipped
            })
        })
    }

    /// Where the cursor stands, in the terms the resize rule speaks of: a wrap pending on the
    /// last column, the cursor after its row's text, the cursor on a row that continues the one
    /// above.
    pub fn cursor_reach(&self) -> (bool, bool, bool) {
        let grid = self.term.grid();
        let cur = self.term.cursor();
        let edge_wrap = grid.pending_wrap() && cur.col + 1 >= grid.cols();
        let row = grid.row(cur.row);
        let past_text =
            !grid.pending_wrap() && row.is_some_and(|row| !row.is_empty() && cur.col >= row.len());
        (
            edge_wrap,
            past_text,
            row.is_some_and(|row| row.is_wrapped()),
        )
    }

    pub fn snap(&self) -> Snap {
        let grid = self.term.grid();
        let (rows, cols) = (grid.rows(), grid.cols());
        let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
        for r in 0..rows {
            for c in 0..cols {
                cells.push(aterm_cell(grid, r, c));
            }
        }
        let cur = self.term.cursor();
        let col = if grid.pending_wrap() {
            cur.col.saturating_add(1)
        } else {
            cur.col
        };
        Snap {
            cols,
            rows,
            cells,
            cursor: (cur.row, col),
            wrap: grid.pending_wrap(),
            cursor_visible: self.term.cursor_visible(),
            alt_screen: self.term.is_alternate_screen(),
        }
    }
}
