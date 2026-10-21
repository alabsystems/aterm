// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The screen writer: a back-buffer of cells laid out as aterm's grid lays them out, and
//! the bytes that turn one frame into the next (docs/DESIGN-amacs-2026-10-19.md §5.4).
//!
//! # Cells
//!
//! A [`Screen`] is rows × cols [`Cell`]s. A cell holds what one grid cell holds: a base
//! character and the characters the grid stores with it (combining marks, variation
//! selectors, the tail of a ZWJ sequence, a flag's second half, a skin tone), its width
//! (1 or 2, and 0 for the right half of a wide cell) and its [`Style`].
//! [`Screen::put`] lays text out a character at a time with
//! [`aterm_grapheme::GridColumns`], the grid's own layout, never with per-cluster widths,
//! so every cell lands where aterm's grid puts it under either ambiguous-width policy.
//!
//! # Any cell can be written on its own
//!
//! [`diff`] rewrites only the cells that changed, each after a cursor move. The grid
//! decides whether a character opens a cell or joins the one before by looking at that
//! cell, so the screen keeps every row in a form in which each cell, written after its
//! left neighbour, lands as itself:
//!
//! - A put continues from the cell on its left as the grid would when the put is
//!   written: a flag's second half joins a lone first half, a skin tone joins a wide
//!   emoji base, a character after a ZWJ joins that cell, and a leading combining mark
//!   or variation selector joins it (VS16 widening it).
//! - A cell just right of a put that would join the put's last cell, were the two
//!   written in a row, is blanked. So is a wide cell a put or a fill cuts in half.
//! - Controls (C0, DEL, C1) are drawn as U+FFFD; a zero-width character with no cell
//!   before it is dropped.
//! - A cell stores at most [`MAX_JOINED`] characters after its base, as the grid's
//!   does. A character that would join a full cell is dropped, a VS16 or VS15 with it:
//!   the grid would still widen or narrow the cell, but its text, written again,
//!   would no longer say so.
//! - A put stops at the first character that does not fit: a wide character at the last
//!   column, or a VS16 that would widen a cell there (the grid does not widen it).
//! - VS15 narrows a wide cell to one column and leaves the column after it blank in the
//!   default style, as the grid does; writing that cell empties that column again, so
//!   the diff rewrites it after the cell when it holds anything else. VS15 after a
//!   regional indicator is dropped: a narrowed half flag would take in another half
//!   flag written beside it.
//!
//! # Frames
//!
//! [`diff`] writes the changed cells left to right in as few bytes as it finds: a
//! relative or absolute cursor move, or a rewrite of the unchanged cells between two
//! changes, whichever is shorter; the shortest SGR from the style before; EL for a row's
//! blank tail. The writer starts from the cursor `prev` shows and ends at the one `next`
//! shows, in its DECSCUSR shape. When the terminal confirmed DEC 2026 a frame that
//! changes cells is one synchronized update; otherwise the cursor is hidden while cells
//! change. Every frame leaves SGR at its default, which the next frame assumes.
//! [`repaint`] draws a whole frame on a terminal in an unknown state: the first frame,
//! and after a resize or a suspension. It first puts back the state an earlier program
//! may have left that moves or reshapes text (a scroll region, origin mode, left and
//! right margins, insert mode, a shifted or graphic charset, SGR), so a later diff
//! builds on exactly the frame it drew.

use aterm_grapheme::{GridColumns, GridStep};
use aterm_types::text_shaping::AmbiguousWidth;

use crate::probe::Caps;
use crate::tty::Size;

const VS15: char = '\u{FE0E}';
const REPLACEMENT: char = '\u{FFFD}';

/// The characters one cell stores after its base: the grid's
/// `CellExtra::MAX_COMBINING`, which `aterm_grapheme::GridColumns` mirrors.
pub const MAX_JOINED: usize = 16;

/// The most rows, and the most columns, a [`Screen`] holds: aterm's grid's
/// `MAX_GRID_ROWS` and `MAX_GRID_COLS`. A terminal reports its own size, so a frame
/// is clamped to this, as the grid is, and a `u16::MAX × u16::MAX` report cannot make
/// it allocate billions of cells.
pub const MAX_SIDE: u16 = 4096;

/// The most cells a [`Screen`] holds: [`MAX_SIDE`] squared.
const MAX_CELLS: usize = MAX_SIDE as usize * MAX_SIDE as usize;

/// A colour from the terminal's own palette, so its theme applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// A palette index: 0-7 and 8-15 are the 16 named colours.
    Indexed(u8),
}

/// SGR attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Attrs(u8);

impl Attrs {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const REVERSE: Self = Self(1 << 4);
    pub const STRIKE: Self = Self(1 << 5);

    /// Each attribute with the SGR parameters that set and reset it.
    const SGR: [(Self, u16, u16); 6] = [
        (Self::BOLD, 1, 22),
        (Self::DIM, 2, 22),
        (Self::ITALIC, 3, 23),
        (Self::UNDERLINE, 4, 24),
        (Self::REVERSE, 7, 27),
        (Self::STRIKE, 9, 29),
    ];

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set these bits name; bits no attribute uses are dropped.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0x3f)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// How a cell is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

/// The cursor's DECSCUSR shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum CursorShape {
    /// The terminal's own default (`CSI 0 SP q`).
    #[default]
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

impl CursorShape {
    const fn decscusr(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::BlinkingBlock => 1,
            Self::SteadyBlock => 2,
            Self::BlinkingUnderline => 3,
            Self::SteadyUnderline => 4,
            Self::BlinkingBar => 5,
            Self::SteadyBar => 6,
        }
    }
}

/// A cell position, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

/// One grid cell (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The base and what the grid stores with it; empty for the right half of a wide
    /// cell, a space for a blank.
    text: String,
    /// 1 or 2; 0 for the right half of a wide cell.
    width: u8,
    /// A wide character VS15 narrowed: it is written two columns wide, then narrowed,
    /// which empties the column after it.
    narrowed: bool,
    style: Style,
}

impl Cell {
    fn blank(style: Style) -> Self {
        Self {
            text: " ".to_owned(),
            width: 1,
            narrowed: false,
            style,
        }
    }

    fn continuation(style: Style) -> Self {
        Self {
            text: String::new(),
            width: 0,
            narrowed: false,
            style,
        }
    }

    /// The base and the characters stored with it; empty for the right half of a wide
    /// cell.
    #[must_use]
    pub fn grapheme(&self) -> &str {
        &self.text
    }

    /// 1 or 2 columns; 0 for the right half of a wide cell.
    #[must_use]
    pub fn width(&self) -> u8 {
        self.width
    }

    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    /// The right half of a wide cell.
    #[must_use]
    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }

    /// A blank in the default style: what the terminal's erase leaves.
    fn is_erased(&self) -> bool {
        self.width == 1 && self.text == " " && self.style == Style::default()
    }

    /// Columns writing the cell covers before the grid settles it: two for a narrowed
    /// cell, its width otherwise.
    fn footprint(&self) -> usize {
        if self.narrowed {
            2
        } else {
            usize::from(self.width)
        }
    }

    fn base(&self) -> Option<char> {
        self.text.chars().next()
    }

    /// It stores [`MAX_JOINED`] characters after its base: the grid keeps no more.
    fn is_full(&self) -> bool {
        self.text.chars().count() > MAX_JOINED
    }
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// A frame: rows × cols cells, the cursor and its shape. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    size: Size,
    ambiguous: AmbiguousWidth,
    cells: Vec<Cell>,
    cursor: Option<Pos>,
    shape: CursorShape,
}

impl Screen {
    /// A blank frame of `size`, each side clamped to [`MAX_SIDE`], laid out under the
    /// terminal's ambiguous-width policy, with no cursor shown.
    #[must_use]
    pub fn new(size: Size, ambiguous: AmbiguousWidth) -> Self {
        let size = Size {
            cols: if size.cols > MAX_SIDE {
                MAX_SIDE
            } else {
                size.cols
            },
            rows: if size.rows > MAX_SIDE {
                MAX_SIDE
            } else {
                size.rows
            },
        };
        // Each side is clamped, so the product never exceeds `MAX_CELLS`; the check says
        // so where the allocation's bound is proved.
        let count = usize::from(size.cols) * usize::from(size.rows);
        let count = if count > MAX_CELLS { MAX_CELLS } else { count };
        Self {
            size,
            ambiguous,
            cells: vec![Cell::blank(Style::default()); count],
            cursor: None,
            shape: CursorShape::Default,
        }
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn ambiguous(&self) -> AmbiguousWidth {
        self.ambiguous
    }

    /// Every cell blank in the default style; the cursor is kept.
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            if !cell.is_erased() {
                *cell = Cell::blank(Style::default());
            }
        }
    }

    /// A blank frame of the new size; the cursor is dropped when it falls outside.
    pub fn resize(&mut self, size: Size) {
        let cursor = self
            .cursor
            .filter(|pos| pos.row < size.rows && pos.col < size.cols);
        *self = Self {
            cursor,
            shape: self.shape,
            ..Self::new(size, self.ambiguous)
        };
    }

    /// The cell at `row`, `col`.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row < self.size.rows && col < self.size.cols {
            self.cells.get(self.index(row, usize::from(col)))
        } else {
            None
        }
    }

    /// Row `row` as text, a wide cell's right half adding nothing.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        (0..self.size.cols)
            .filter_map(|col| self.cell(row, col))
            .map(Cell::grapheme)
            .collect()
    }

    /// Where the cursor is shown; `None` hides it.
    #[must_use]
    pub fn cursor(&self) -> Option<Pos> {
        self.cursor
    }

    /// Show the cursor at `pos`, or hide it. A position outside the frame hides it.
    pub fn set_cursor(&mut self, pos: Option<Pos>) {
        self.cursor = pos.filter(|pos| pos.row < self.size.rows && pos.col < self.size.cols);
    }

    #[must_use]
    pub fn cursor_shape(&self) -> CursorShape {
        self.shape
    }

    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.shape = shape;
    }

    fn cols(&self) -> usize {
        usize::from(self.size.cols)
    }

    /// Where `row`, `col` sits in `cells`. Every caller keeps `col` inside the row, so
    /// the sum never saturates.
    fn index(&self, row: u16, col: usize) -> usize {
        (usize::from(row) * usize::from(self.size.cols)).saturating_add(col)
    }

    fn at(&mut self, row: u16, col: usize) -> &mut Cell {
        let index = self.index(row, col);
        &mut self.cells[index]
    }

    fn get(&self, row: u16, col: usize) -> &Cell {
        &self.cells[self.index(row, col)]
    }

    /// The first column of the cell covering `col`.
    fn head(&self, row: u16, col: usize) -> usize {
        if col > 0 && self.get(row, col).is_continuation() {
            col - 1
        } else {
            col
        }
    }

    /// Blank both halves of a wide cell whose right half is `col`: what is written at
    /// `col` cuts it in two.
    fn split(&mut self, row: u16, col: usize) {
        if col < self.cols() && col > 0 && self.get(row, col).is_continuation() {
            let style = self.get(row, col - 1).style;
            *self.at(row, col - 1) = Cell::blank(style);
            *self.at(row, col) = Cell::blank(style);
        }
    }

    /// Blank what a put left half of in `from..=to`: a right half whose wide cell was
    /// overwritten, a wide cell whose right half was.
    fn mend(&mut self, row: u16, from: usize, to: usize) {
        let cols = self.cols();
        for col in from..=to.min(cols.saturating_sub(1)) {
            let cell = self.get(row, col);
            let orphan = if cell.is_continuation() {
                col == 0 || self.get(row, col - 1).width != 2
            } else {
                let right = col.saturating_add(1);
                cell.width == 2 && (right >= cols || !self.get(row, right).is_continuation())
            };
            if orphan {
                let style = cell.style;
                *self.at(row, col) = Cell::blank(style);
            }
        }
    }

    /// Write `text` in `style` from `row`, `col`, as the grid lays it out (see the module
    /// docs), and return the column after it: where the terminal's cursor stands once it
    /// is written. Text past the right edge is not drawn.
    pub fn put(&mut self, row: u16, col: u16, text: &str, style: Style) -> u16 {
        if row >= self.size.rows || col >= self.size.cols {
            return col.min(self.size.cols);
        }
        let cols = self.cols();
        let start = usize::from(col);
        self.split(row, start);

        // The walk starts from the cell on the left, as the grid's does: the characters
        // it stores decide what the put's first character joins.
        let mut walker = GridColumns::with_ambiguous_width(self.ambiguous);
        let mut origin = start;
        let mut current = None;
        if start > 0 {
            let head = self.head(row, start - 1);
            let left = self.get(row, head);
            // A narrowed cell takes nothing written beside it: it is not wide, ends in no
            // ZWJ, and holds no lone regional indicator (VS15 after one is dropped).
            if !left.narrowed {
                for c in left.text.chars() {
                    walker.step(c);
                }
                origin = head;
                current = Some(head);
                debug_assert_eq!(origin.saturating_add(walker.column()), start);
            }
        }

        for c in text.chars() {
            let c = if c.is_control() { REPLACEMENT } else { c };
            let mut next = walker.clone();
            match next.step(c) {
                // Column sums stay inside the row (a put stops at its edge), so none
                // saturates.
                GridStep::Opens { col, width } => {
                    let at = origin.saturating_add(col);
                    if at.saturating_add(width) > cols {
                        break;
                    }
                    *self.at(row, at) = Cell {
                        text: c.to_string(),
                        width: u8::try_from(width).unwrap_or(1),
                        narrowed: false,
                        style,
                    };
                    if width == 2 {
                        *self.at(row, at.saturating_add(1)) = Cell::continuation(style);
                    }
                    current = Some(at);
                }
                GridStep::Folds { col } => {
                    let cell = self.at(row, origin.saturating_add(col));
                    if cell.is_full() {
                        continue;
                    }
                    cell.text.push(c);
                }
                GridStep::Joins { widened } => {
                    // With no cell before it, a zero-width character is dropped; so is
                    // one that would join a full cell, the walk left as if it never came.
                    let Some(at) = current else {
                        continue;
                    };
                    let cell = self.get(row, at);
                    if cell.is_full() {
                        continue;
                    }
                    if c == VS15 && cell.width == 2 {
                        if cell.base().is_some_and(is_regional_indicator) {
                            continue;
                        }
                        let cell = self.at(row, at);
                        cell.text.push(c);
                        cell.width = 1;
                        cell.narrowed = true;
                        let right = at.saturating_add(1);
                        *self.at(row, right) = Cell::blank(Style::default());
                        current = Some(right);
                    } else if widened {
                        // The grid widens nothing at the last column.
                        if at.saturating_add(2) > cols {
                            break;
                        }
                        let cell = self.at(row, at);
                        cell.text.push(c);
                        cell.width = 2;
                        let style = cell.style;
                        *self.at(row, at.saturating_add(1)) = Cell::continuation(style);
                    } else {
                        self.at(row, at).text.push(c);
                    }
                }
            }
            walker = next;
        }
        let end = origin.saturating_add(walker.column()).min(cols);
        self.mend(row, start.saturating_sub(1), end);

        // The cell after the put would join its last cell were the two written in a row.
        if end < cols {
            let right = self.get(row, end);
            if let Some(base) = right.base()
                && !right.is_continuation()
                && matches!(walker.clone().step(base), GridStep::Folds { .. })
            {
                let (style, width) = (right.style, usize::from(right.width));
                for col in end..end.saturating_add(width) {
                    *self.at(row, col) = Cell::blank(style);
                }
            }
        }
        u16::try_from(end).unwrap_or(self.size.cols)
    }

    /// Blank `from..to` of `row` in `style`. A wide cell the range cuts in half is
    /// blanked whole.
    pub fn fill(&mut self, row: u16, from: u16, to: u16, style: Style) {
        let to = to.min(self.size.cols);
        if row >= self.size.rows || from >= to {
            return;
        }
        let (from, to) = (usize::from(from), usize::from(to));
        self.split(row, from);
        self.split(row, to);
        for col in from..to {
            *self.at(row, col) = Cell::blank(style);
        }
    }

    /// Restyle the cells of `row` that `from..to` touches, whole, keeping their text.
    pub fn set_style(&mut self, row: u16, from: u16, to: u16, style: Style) {
        let to = usize::from(to.min(self.size.cols));
        if row >= self.size.rows || usize::from(from) >= to {
            return;
        }
        let mut col = self.head(row, usize::from(from));
        while col < to {
            let width = usize::from(self.get(row, col).width).max(1);
            let next = col.saturating_add(width);
            for part in col..next {
                self.at(row, part).style = style;
            }
            col = next;
        }
    }
}

/// The bytes that turn `prev`, the frame the terminal shows, into `next`; none when
/// nothing changed. A frame of another size or ambiguous-width policy is a
/// [`repaint`]. See the module docs.
#[must_use]
pub fn diff(prev: &Screen, next: &Screen, caps: &Caps) -> Vec<u8> {
    if prev.size != next.size || prev.ambiguous != next.ambiguous {
        return repaint(next, caps);
    }
    Painter::paint(prev, next, caps.sync_2026, false)
}

/// The bytes that draw `next` whole on a terminal in an unknown state: the charset and
/// SGR reset, the screen cleared, then every cell that is not blank, the cursor and its
/// shape.
#[must_use]
pub fn repaint(next: &Screen, caps: &Caps) -> Vec<u8> {
    let blank = Screen::new(next.size, next.ambiguous);
    Painter::paint(&blank, next, caps.sync_2026, true)
}

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";
const HIDE: &[u8] = b"\x1b[?25l";
const SHOW: &[u8] = b"\x1b[?25h";
/// What an earlier program may have left that would move or reshape what the frame
/// writes, put back, then the screen erased: G0 to ASCII and shifted in, SGR reset,
/// insert mode off, origin mode off, the left and right margins off (DECLRMM) and the
/// scroll region the whole screen, so every CUP and `\r\n` lands where the painter
/// counts; then home and erase.
const CLEAR: &[u8] = b"\x1b(B\x0f\x1b[m\x1b[4l\x1b[?6l\x1b[?69l\x1b[r\x1b[H\x1b[2J";
const ERASE_LINE: &[u8] = b"\x1b[K";

/// Where the writer is: the terminal's cursor (`None` when unknown, or parked past the
/// last column where the next character would wrap), and its SGR.
struct Painter<'a> {
    next: &'a Screen,
    out: Vec<u8>,
    at: Option<Pos>,
    sgr: Style,
}

impl Painter<'_> {
    fn paint(prev: &Screen, next: &Screen, sync: bool, full: bool) -> Vec<u8> {
        let cells = full || prev.cells != next.cells;
        if !cells && prev.cursor == next.cursor && prev.shape == next.shape {
            return Vec::new();
        }
        let mut p = Painter {
            next,
            out: Vec::new(),
            // The terminal shows `prev`, so its cursor stands where `prev` shows one.
            at: if full { None } else { prev.cursor },
            sgr: Style::default(),
        };
        // A synchronized update shows no cursor mid-frame; without one the cursor is
        // hidden while cells are written, so it does not flit across the screen.
        let sync = sync && cells;
        let mut visible = !full && prev.cursor.is_some();
        if sync {
            p.out.extend_from_slice(BSU);
        }
        if full || (cells && visible && !sync) {
            p.out.extend_from_slice(HIDE);
            visible = false;
        }
        if cells {
            if full {
                p.out.extend_from_slice(CLEAR);
                p.at = Some(Pos { row: 0, col: 0 });
            }
            let cols = usize::from(next.size.cols);
            for row in 0..next.size.rows {
                let start = usize::from(row) * cols;
                let span = start..start.saturating_add(cols);
                if prev.cells[span.clone()] != next.cells[span] {
                    p.row(prev, row);
                }
            }
            p.style(Style::default());
        }
        if full || prev.shape != next.shape {
            p.out
                .extend_from_slice(format!("\x1b[{} q", next.shape.decscusr()).as_bytes());
        }
        match next.cursor {
            Some(pos) => {
                p.goto(pos);
                if !visible {
                    p.out.extend_from_slice(SHOW);
                }
            }
            None if visible => p.out.extend_from_slice(HIDE),
            None => {}
        }
        if sync {
            p.out.extend_from_slice(ESU);
        }
        p.out
    }

    /// Rewrite the cells of `row` that differ from `prev`'s.
    fn row(&mut self, prev: &Screen, row: u16) {
        let next = self.next;
        let cols = usize::from(next.size.cols);
        let mut need: Vec<bool> = (0..cols)
            .map(|col| next.get(row, col) != prev.get(row, col))
            .collect();
        for col in 0..cols {
            if need.get(col) != Some(&true) {
                continue;
            }
            // A changed right half is written with its wide cell (a frame's first column
            // is never one); writing a narrowed cell empties the column after it, which
            // is written again when it holds more.
            let cell = next.get(row, col);
            if cell.is_continuation()
                && let Some(left) = col.checked_sub(1).and_then(|left| need.get_mut(left))
            {
                *left = true;
            }
            let right = col.saturating_add(1);
            if cell.narrowed
                && right < cols
                && !next.get(row, right).is_erased()
                && let Some(right) = need.get_mut(right)
            {
                *right = true;
            }
        }
        // The row's blank tail, which one EL can write.
        let mut tail = cols;
        while tail > 0 && next.get(row, tail - 1).is_erased() {
            tail -= 1;
        }
        let needed = |from: usize| {
            (from..cols)
                .find(|&col| need.get(col) == Some(&true) && !next.get(row, col).is_continuation())
        };
        let mut col = 0;
        while let Some(at) = needed(col) {
            if at >= tail {
                self.erase_tail(row, at, &need);
                return;
            }
            self.reach(row, at);
            self.cell(row, at);
            col = at.saturating_add(usize::from(next.get(row, at).width));
        }
    }

    /// Bring the cursor to `at` in `row`: by rewriting the unchanged cells between it and
    /// `at` when that is shorter than a move.
    fn reach(&mut self, row: u16, at: usize) {
        let next = self.next;
        if let Some(Pos { row: r, col }) = self.at
            && r == row
            && usize::from(col) < at
        {
            let from = usize::from(col);
            let mut cost = 0;
            let mut sgr = self.sgr;
            let mut gap = from;
            while gap < at {
                let cell = next.get(row, gap);
                if cell.is_continuation() || cell.narrowed {
                    cost = usize::MAX;
                    break;
                }
                cost = cost
                    .saturating_add(sgr_bytes(sgr, cell.style).len())
                    .saturating_add(cell.text.len());
                sgr = cell.style;
                gap = gap.saturating_add(usize::from(cell.width));
            }
            let pos = Pos {
                row,
                col: u16::try_from(at).unwrap_or(u16::MAX),
            };
            if gap == at && cost <= moves(self.at, pos).len() {
                let mut through = from;
                while through < at {
                    self.cell(row, through);
                    through = through.saturating_add(usize::from(next.get(row, through).width));
                }
                return;
            }
        }
        self.goto(Pos {
            row,
            col: u16::try_from(at).unwrap_or(u16::MAX),
        });
    }

    /// The cells from `at` to the end of `row` are blank in the default style: one EL,
    /// or the few spaces that are shorter.
    fn erase_tail(&mut self, row: u16, at: usize, need: &[bool]) {
        let last = (at..need.len())
            .rev()
            .find(|&col| need.get(col) == Some(&true))
            .unwrap_or(at);
        let spaces = last.saturating_sub(at).saturating_add(1);
        self.reach(row, at);
        self.style(Style::default());
        if spaces < ERASE_LINE.len() {
            for col in at..=last {
                self.cell(row, col);
            }
        } else {
            self.out.extend_from_slice(ERASE_LINE);
        }
    }

    /// Write the cell at `col` (a cell's first column), the cursor where it is.
    fn cell(&mut self, row: u16, col: usize) {
        let cell = self.next.get(row, col);
        self.style(cell.style);
        self.out.extend_from_slice(cell.text.as_bytes());
        let end = col.saturating_add(cell.footprint());
        self.at = (end < self.next.cols()).then(|| Pos {
            row,
            col: u16::try_from(end).unwrap_or(u16::MAX),
        });
    }

    fn style(&mut self, style: Style) {
        self.out.extend(sgr_bytes(self.sgr, style));
        self.sgr = style;
    }

    fn goto(&mut self, pos: Pos) {
        self.out.extend(moves(self.at, pos));
        self.at = Some(pos);
    }
}

/// The shortest bytes that move the cursor from `from` (`None`: unknown) to `to`.
fn moves(from: Option<Pos>, to: Pos) -> Vec<u8> {
    let absolute = csi_pos(to);
    let Some(from) = from else {
        return absolute;
    };
    if from == to {
        return Vec::new();
    }
    let mut best = absolute;
    let mut consider = |bytes: Vec<u8>| {
        if bytes.len() < best.len() {
            best = bytes;
        }
    };
    // Widened, so a 1-based column or the row after one cannot overflow.
    let (from_row, to_row) = (u32::from(from.row), u32::from(to.row));
    if from.row == to.row {
        let (a, b) = (u32::from(from.col), u32::from(to.col));
        if b > a {
            consider(csi_n(b - a, b'C'));
        } else {
            consider(csi_n(a - b, b'D'));
            let mut cr = b"\r".to_vec();
            if b > 0 {
                cr.extend(csi_n(b, b'C'));
            }
            consider(cr);
        }
        consider(csi_n(b + 1, b'G'));
    } else if from.col == to.col {
        if to_row > from_row {
            consider(csi_n(to_row - from_row, b'B'));
        } else {
            consider(csi_n(from_row - to_row, b'A'));
        }
    } else if to.col == 0 && to_row == from_row + 1 {
        consider(b"\r\n".to_vec());
    }
    best
}

/// `CSI n <final>`, the 1 left out.
fn csi_n(n: u32, final_byte: u8) -> Vec<u8> {
    let mut out = b"\x1b[".to_vec();
    if n != 1 {
        out.extend_from_slice(n.to_string().as_bytes());
    }
    out.push(final_byte);
    out
}

/// CUP to `to`, its defaults left out.
fn csi_pos(to: Pos) -> Vec<u8> {
    let mut out = b"\x1b[".to_vec();
    if to.row != 0 || to.col != 0 {
        out.extend_from_slice((u32::from(to.row) + 1).to_string().as_bytes());
    }
    if to.col != 0 {
        out.push(b';');
        out.extend_from_slice((u32::from(to.col) + 1).to_string().as_bytes());
    }
    out.push(b'H');
    out
}

fn color_params(color: Color, background: bool, out: &mut Vec<u16>) {
    let base = if background { 10 } else { 0 };
    match color {
        Color::Default => out.push(39 + base),
        Color::Indexed(n @ 0..=7) => out.push(30 + base + u16::from(n)),
        Color::Indexed(n @ 8..=15) => out.push(90 + base + u16::from(n - 8)),
        Color::Indexed(n) => out.extend([38 + base, 5, u16::from(n)]),
    }
}

/// The shortest SGR that takes the terminal from `from` to `to`: the changes alone, or
/// a reset and everything `to` sets.
fn sgr_bytes(from: Style, to: Style) -> Vec<u8> {
    if from == to {
        return Vec::new();
    }
    // The changes alone. SGR 22 resets bold and dim together.
    let mut changes = Vec::new();
    let off = from.attrs.difference(to.attrs);
    let mut on = to.attrs.difference(from.attrs);
    if off.contains(Attrs::BOLD) || off.contains(Attrs::DIM) {
        changes.push(22);
        on = on.union(Attrs::from_bits(
            to.attrs.bits() & (Attrs::BOLD.bits() | Attrs::DIM.bits()),
        ));
    }
    for (attr, _, reset) in Attrs::SGR {
        if reset != 22 && off.contains(attr) {
            changes.push(reset);
        }
    }
    for (attr, set, _) in Attrs::SGR {
        if on.contains(attr) {
            changes.push(set);
        }
    }
    if from.fg != to.fg {
        color_params(to.fg, false, &mut changes);
    }
    if from.bg != to.bg {
        color_params(to.bg, true, &mut changes);
    }
    // A reset and everything `to` sets.
    let mut reset = Vec::new();
    if to != Style::default() {
        reset.push(0);
        for (attr, set, _) in Attrs::SGR {
            if to.attrs.contains(attr) {
                reset.push(set);
            }
        }
        if to.fg != Color::Default {
            color_params(to.fg, false, &mut reset);
        }
        if to.bg != Color::Default {
            color_params(to.bg, true, &mut reset);
        }
    }
    let (changes, reset) = (sgr_encode(&changes), sgr_encode(&reset));
    if reset.len() < changes.len() {
        reset
    } else {
        changes
    }
}

fn sgr_encode(params: &[u16]) -> Vec<u8> {
    let mut out = b"\x1b[".to_vec();
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            out.push(b';');
        }
        out.extend_from_slice(param.to_string().as_bytes());
    }
    out.push(b'm');
    out
}

#[cfg(test)]
mod tests {
    use aterm_types::text_shaping::AmbiguousWidth;

    use super::{Attrs, Color, MAX_JOINED, MAX_SIDE, Pos, Screen, Style, moves, sgr_bytes};
    use crate::tty::Size;

    fn screen(cols: u16) -> Screen {
        Screen::new(Size { cols, rows: 2 }, AmbiguousWidth::Single)
    }

    #[test]
    fn put_lays_text_out_as_the_grid_does() {
        let mut s = screen(10);
        let plain = Style::default();
        assert_eq!(s.put(0, 0, "a中b", plain), 4);
        assert_eq!(s.row_text(0), "a中b      ");
        // A flag's halves put apart meet in one cell, as the grid joins them.
        let mut s = screen(10);
        assert_eq!(s.put(0, 0, "\u{1F1FA}", plain), 2);
        assert_eq!(s.put(0, 2, "\u{1F1F8}x", plain), 3);
        assert_eq!(s.row_text(0), "\u{1F1FA}\u{1F1F8}x       ");
        // A wide character that does not fit is not drawn; nor is what follows it.
        let mut s = screen(3);
        assert_eq!(s.put(0, 1, "a中b", plain), 2);
        assert_eq!(s.row_text(0), " a ");
        // VS15 narrows a wide emoji and leaves the column after it blank.
        let mut s = screen(6);
        assert_eq!(s.put(0, 0, "\u{231A}\u{FE0E}x", plain), 3);
        assert_eq!(s.cell(0, 0).unwrap().width(), 1);
        assert_eq!(s.row_text(0), "\u{231A}\u{FE0E} x   ");
        // Controls are drawn as U+FFFD.
        let mut s = screen(4);
        s.put(0, 0, "\t\x1b", plain);
        assert_eq!(s.row_text(0), "\u{FFFD}\u{FFFD}  ");
        // A cell keeps 16 characters after its base, as the grid does: the rest are
        // dropped, a VS16 that would widen it with them.
        let mut s = screen(4);
        let marks = "\u{301}".repeat(20);
        assert_eq!(s.put(0, 0, &format!("e{marks}"), plain), 1);
        assert_eq!(
            s.cell(0, 0).unwrap().grapheme().chars().count(),
            1 + MAX_JOINED
        );
        assert_eq!(s.put(0, 0, &format!("\u{2764}{marks}\u{FE0F}x"), plain), 2);
        assert_eq!(s.cell(0, 0).unwrap().width(), 1);
        assert_eq!(s.cell(0, 1).unwrap().grapheme(), "x");
        assert_eq!(
            s.put(0, 1, "\u{301}", plain),
            1,
            "a full cell takes nothing more"
        );
        assert_eq!(
            s.cell(0, 0).unwrap().grapheme().chars().count(),
            1 + MAX_JOINED
        );
        assert_eq!(s.cell(0, 1).unwrap().grapheme(), "x");
    }

    #[test]
    fn a_cell_that_would_join_the_put_before_it_is_blanked() {
        let plain = Style::default();
        let mut s = screen(6);
        s.put(0, 2, "\u{1F1F8}", plain);
        s.put(0, 0, "\u{1F1FA}", plain);
        assert_eq!(s.row_text(0), "\u{1F1FA}    ");
        // Cutting a wide cell in half blanks it whole.
        let mut s = screen(6);
        s.put(0, 0, "中文", plain);
        s.put(0, 1, "x", plain);
        assert_eq!(s.row_text(0), " x文  ");
        s.fill(0, 3, 4, plain);
        assert_eq!(s.row_text(0), " x    ");
    }

    #[test]
    fn sgr_is_the_shorter_of_the_changes_and_a_reset() {
        let bold_red = Style {
            fg: Color::Indexed(1),
            attrs: Attrs::BOLD,
            ..Style::default()
        };
        assert_eq!(sgr_bytes(Style::default(), bold_red), b"\x1b[1;31m");
        assert_eq!(sgr_bytes(bold_red, Style::default()), b"\x1b[m");
        let dim_red = Style {
            attrs: Attrs::DIM,
            ..bold_red
        };
        assert_eq!(sgr_bytes(bold_red, dim_red), b"\x1b[22;2m");
        let grey = Style {
            bg: Color::Indexed(236),
            ..Style::default()
        };
        assert_eq!(sgr_bytes(Style::default(), grey), b"\x1b[48;5;236m");
        assert_eq!(sgr_bytes(grey, grey), b"");
    }

    #[test]
    fn a_frame_is_clamped_to_the_grid_size_limit() {
        let huge = Size {
            cols: u16::MAX,
            rows: 2,
        };
        let mut s = Screen::new(huge, AmbiguousWidth::Single);
        assert_eq!(s.size().cols, MAX_SIDE);
        assert_eq!(s.size().rows, 2);
        assert!(s.cell(1, MAX_SIDE - 1).is_some());
        assert!(s.cell(1, MAX_SIDE).is_none());
        s.set_cursor(Some(Pos { row: 1, col: 2 }));
        s.resize(Size {
            cols: 3,
            rows: u16::MAX,
        });
        assert_eq!(s.size().rows, MAX_SIDE);
        assert_eq!(s.size().cols, 3);
        assert_eq!(s.cursor(), Some(Pos { row: 1, col: 2 }));
    }

    #[test]
    fn moves_take_the_shortest_spelling() {
        let at = |row, col| Pos { row, col };
        assert_eq!(moves(None, at(0, 0)), b"\x1b[H");
        assert_eq!(moves(None, at(4, 0)), b"\x1b[5H");
        assert_eq!(moves(None, at(4, 9)), b"\x1b[5;10H");
        assert_eq!(moves(Some(at(4, 9)), at(4, 10)), b"\x1b[C");
        assert_eq!(moves(Some(at(4, 9)), at(4, 0)), b"\r");
        assert_eq!(moves(Some(at(4, 9)), at(5, 0)), b"\r\n");
        assert_eq!(moves(Some(at(4, 9)), at(6, 9)), b"\x1b[2B");
        assert_eq!(moves(Some(at(4, 9)), at(4, 9)), b"");
    }
}
