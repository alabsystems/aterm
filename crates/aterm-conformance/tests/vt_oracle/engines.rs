// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared screen-state projection for `aterm` and `astream-term` oracle comparison.

use astream_term::Record;
use astream_term::screen::{Color, Folder, attr};
use aterm_core::grid::{CellFlags, Grid};
use aterm_core::terminal::Terminal;
use std::fmt::Write as _;

/// A colour both engines can name: the terminal default or a palette index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Default,
    Indexed(u8),
    Other,
}

/// One cell's rendition in the shared vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    pub fg: Paint,
    pub bg: Paint,
    pub bold: bool,
    pub underline: bool,
    pub reverse: bool,
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
    /// Row `r`'s text, trailing blanks trimmed.
    pub fn row_text(&self, r: u16) -> String {
        let start = usize::from(r) * usize::from(self.cols);
        let line: String = self.cells[start..start + usize::from(self.cols)]
            .iter()
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

fn paint_of(c: Color) -> Paint {
    match c {
        Color::Default => Paint::Default,
        Color::Indexed(n) => Paint::Indexed(n),
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
                        fg: paint_of(cell.fg),
                        bg: paint_of(cell.bg),
                        bold: cell.attrs & attr::BOLD != 0,
                        underline: cell.attrs & attr::UNDERLINE != 0,
                        reverse: cell.attrs & attr::REVERSE != 0,
                        extra: u16::from(
                            cell.attrs & !(attr::BOLD | attr::UNDERLINE | attr::REVERSE),
                        ),
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

const UNDERLINE_BITS: u16 =
    CellFlags::UNDERLINE.0 | CellFlags::DOUBLE_UNDERLINE.0 | CellFlags::CURLY_UNDERLINE.0;
const MODELLED_BITS: u16 = CellFlags::BOLD.0 | CellFlags::INVERSE.0 | UNDERLINE_BITS;

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
        Paint::Other
    };
    let bg = if colors.bg_is_default() {
        Paint::Default
    } else if colors.bg_is_indexed() {
        Paint::Indexed(colors.bg_index())
    } else {
        Paint::Other
    };
    let flags = cell.flags().0;
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
            extra: flags & !MODELLED_BITS,
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
