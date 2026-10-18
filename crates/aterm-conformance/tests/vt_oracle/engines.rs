// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two engines, projected onto the state both of them model: per cell the
//! character, the foreground and background as a palette index or the
//! default, and bold/underline/reverse; the cursor, with a pending wrap
//! spelled astream-term's way (`col == cols`); cursor visibility; and which
//! buffer is up.
//!
//! The projection is deliberately lossless for that state and blind to the
//! rest. aterm's cell flags outside the three astream-term models are kept in
//! [`Ink::extra`] rather than dropped, so a program that turns one on (italic,
//! blink, the wide bit of a CJK glyph) shows up as a mismatch against
//! astream-term's zero instead of passing silently: the subset boundary is
//! enforced by the comparison, not trusted to the generator. (That is how the
//! oracle caught aterm turning a truncated `38;5` into blink.)

use astream_term::Record;
use astream_term::screen::{Color, Folder, attr};
use aterm_core::grid::{CellFlags, Grid};
use aterm_core::terminal::Terminal;
use std::fmt::Write as _;

/// A colour both engines can name: the terminal default or a palette index.
/// `Other` is aterm's truecolour, which no program in the subset selects.
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
    /// aterm-only attribute bits (see the module header); always 0 for
    /// astream-term.
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
    /// `(row, col)`, 0-based; `col == cols` while a wrap is pending.
    pub cursor: (u16, u16),
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

// ---------------------------------------------------------------------------
// astream-term
// ---------------------------------------------------------------------------

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
                        // FAINT is astream-term's speculative-echo mark; no SGR
                        // sets it, so a folded screen never carries it.
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
            cursor_visible: s.cursor_visible(),
            alt_screen: s.is_altscreen(),
        }
    }
}

// ---------------------------------------------------------------------------
// aterm
// ---------------------------------------------------------------------------

/// Every underline style aterm stores (single, double, curly, dotted, dashed
/// are combinations of these three bits). astream-term models one underline
/// and folds `4:n` onto it, so any of them projects to `underline`.
const UNDERLINE_BITS: u16 =
    CellFlags::UNDERLINE.0 | CellFlags::DOUBLE_UNDERLINE.0 | CellFlags::CURLY_UNDERLINE.0;
const MODELLED_BITS: u16 = CellFlags::BOLD.0 | CellFlags::INVERSE.0 | UNDERLINE_BITS;

/// The production engine, advanced chunk by chunk.
pub struct Aterm {
    term: Terminal,
}

fn aterm_cell(grid: &Grid, r: u16, c: u16) -> CellView {
    let Some(cell) = grid.cell(r, c) else {
        // A row shorter than the grid (a lazily materialised row) is blank.
        return CellView {
            ch: ' ',
            ink: Ink::PLAIN,
        };
    };
    // A cell's colours are inline, so they are the truth.
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
        // aterm stores an erased cell as NUL in some paths and as a space in
        // others; both are a blank on screen, which is what astream-term says.
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
        // aterm parks a pending wrap ON the last column with a flag beside it;
        // astream-term parks it one past. Same state, two spellings.
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
            cursor_visible: self.term.cursor_visible(),
            alt_screen: self.term.is_alternate_screen(),
        }
    }
}
