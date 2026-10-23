// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The terminal grid's layout of a line, one character at a time.

use aterm_types::text_shaping::AmbiguousWidth;

use crate::emoji_presentation::is_vs16_emoji_capable;
use crate::grapheme_iter::{GraphemeClusters, GraphemeIndices};
use crate::tables::width as unicode_tables;
use crate::width::{is_emoji_modifier_base, is_regional_indicator, is_skin_tone_modifier};

const VS15: char = '\u{FE0E}';
const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';

/// Characters one cell stores after its base (`aterm_grid`'s
/// `CellExtra::MAX_COMBINING`).
const MAX_JOINED: u8 = 16;

/// The cell the last printing character opened, as the grid holds it.
#[derive(Clone, Copy, Debug)]
struct Cell {
    /// Its first column.
    col: usize,
    base: char,
    /// Drawn wide: VS16 sets it, VS15 clears it, and only a wide cell takes a
    /// skin-tone modifier.
    wide: bool,
    /// Regional indicators stored in it, the base included: a flag is two.
    ris: u8,
    /// Characters stored after the base.
    joined: u8,
    /// The last stored character is a ZWJ (`should_combine_with_previous_zwj`).
    ends_zwj: bool,
}

impl Cell {
    #[inline]
    const fn open(col: usize, base: char, wide: bool) -> Self {
        Self {
            col,
            base,
            wide,
            ris: is_regional_indicator(base) as u8,
            joined: 0,
            ends_zwj: false,
        }
    }

    /// Store `c` after the base, as `CellExtra::add_combining` does.
    #[inline]
    const fn store(&mut self, c: char) {
        if self.joined < MAX_JOINED {
            self.joined += 1;
            self.ends_zwj = c == ZWJ;
            self.ris = self.ris.saturating_add(is_regional_indicator(c) as u8);
        }
    }

    /// Whether the grid folds printing character `c` into this cell rather
    /// than open a new one, in `write_char`'s order: a skin-tone modifier onto
    /// a wide `Emoji_Modifier_Base`, a flag's second regional indicator onto
    /// its first, and any non-ASCII character after a stored ZWJ.
    #[inline]
    const fn folds(&self, c: char) -> bool {
        (is_skin_tone_modifier(c) && self.wide && is_emoji_modifier_base(self.base))
            || (is_regional_indicator(c) && is_regional_indicator(self.base) && self.ris < 2)
            || (self.ends_zwj && !c.is_ascii())
    }
}

/// What one character did to the grid (see [`GridColumns::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridStep {
    /// A printing character opened a cell of its own at column `col`, `width` columns wide.
    Opens {
        /// The cell's first column.
        col: usize,
        /// Its width, 1 or 2.
        width: usize,
    },
    /// A printing character the grid folded into the cell before it, at column `col`.
    Folds {
        /// The first column of the cell it joined.
        col: usize,
    },
    /// A zero-width character stored in the cell before it.
    Joins {
        /// VS16 widened that cell from 1 column to 2, moving the cursor one.
        widened: bool,
    },
}

impl GridStep {
    /// Columns the cursor advanced.
    #[must_use]
    #[inline]
    pub const fn advance(self) -> usize {
        match self {
            Self::Opens { width, .. } => width,
            Self::Folds { .. } | Self::Joins { widened: false } => 0,
            Self::Joins { widened: true } => 1,
        }
    }
}

/// The grid's layout of one line, fed a character at a time from the line's first column.
#[derive(Clone, Debug, Default)]
pub struct GridColumns {
    /// The cursor column.
    cursor: usize,
    /// The cell a joining character lands in; `None` before the first one.
    cell: Option<Cell>,
    /// How wide the grid draws an East Asian Ambiguous character: the
    /// terminal's `ambiguous_width` policy the line was written under.
    ambiguous: AmbiguousWidth,
}

impl GridColumns {
    /// A walk from a line's first column, East Asian Ambiguous characters one cell wide.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self::with_ambiguous_width(AmbiguousWidth::Single)
    }

    /// A walk from a line's first column.
    #[must_use]
    #[inline]
    pub const fn with_ambiguous_width(ambiguous: AmbiguousWidth) -> Self {
        Self {
            cursor: 0,
            cell: None,
            ambiguous,
        }
    }

    /// The cursor column: the cells everything fed so far occupies.
    #[must_use]
    #[inline]
    pub const fn column(&self) -> usize {
        self.cursor
    }

    /// Lay out `c` after everything fed so far.
    #[inline]
    pub fn step(&mut self, c: char) -> GridStep {
        let width = match self.ambiguous {
            AmbiguousWidth::Double => unicode_tables::char_width_cjk(c),
            _ => unicode_tables::char_width(c),
        };
        match self.cell.as_mut() {
            Some(cell) if width == 0 => {
                cell.store(c);
                if c == VS16 && !cell.wide && is_vs16_emoji_capable(cell.base) {
                    cell.wide = true;
                    self.cursor = self.cursor.saturating_add(1);
                    return GridStep::Joins { widened: true };
                }
                if c == VS15 && cell.wide {
                    // The glyph narrows; the cursor stays past the emptied
                    // column, which is now the cell before it.
                    *cell = Cell::open(cell.col.saturating_add(1), ' ', false);
                }
                GridStep::Joins { widened: false }
            }
            Some(cell) if cell.folds(c) => {
                cell.store(c);
                GridStep::Folds { col: cell.col }
            }
            None if width == 0 => GridStep::Joins { widened: false },
            _ => {
                let col = self.cursor;
                self.cell = Some(Cell::open(col, c, width >= 2));
                self.cursor = self.cursor.saturating_add(width);
                GridStep::Opens { col, width }
            }
        }
    }

    /// Lay out every character of `s`; returns the columns the cursor advanced.
    #[inline]
    pub fn advance_str(&mut self, s: &str) -> usize {
        let before = self.cursor;
        for c in s.chars() {
            self.step(c);
        }
        self.cursor.saturating_sub(before)
    }
}

/// The number of grid cells a terminal lays `s` out in, written from the first column of a line.
#[must_use]
#[inline]
pub fn str_grid_columns(s: &str) -> usize {
    str_grid_columns_with(s, AmbiguousWidth::Single)
}

/// [`str_grid_columns`] under the terminal's `ambiguous_width` policy.
#[must_use]
#[inline]
pub fn str_grid_columns_with(s: &str, ambiguous: AmbiguousWidth) -> usize {
    GridColumns::with_ambiguous_width(ambiguous).advance_str(s)
}

/// One grapheme cluster and the grid columns it is drawn in (see [`grid_graphemes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridGrapheme<'a> {
    /// The cluster.
    pub text: &'a str,
    /// Its byte offset in the line.
    pub byte_offset: usize,
    /// The first column it is drawn in.
    pub start_col: usize,
    /// The cursor column after it.
    pub end_col: usize,
}

/// Iterator over a line's grapheme clusters with their grid columns.
#[derive(Clone, Debug)]
pub struct GridGraphemes<'a> {
    clusters: GraphemeIndices<'a>,
    grid: GridColumns,
}

impl<'a> Iterator for GridGraphemes<'a> {
    type Item = GridGrapheme<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let (byte_offset, text) = self.clusters.next()?;
        let mut start_col = self.grid.column();
        let mut chars = text.chars();
        if let Some(GridStep::Folds { col }) = chars.next().map(|c| self.grid.step(c)) {
            start_col = col;
        }
        for c in chars {
            self.grid.step(c);
        }
        Some(GridGrapheme {
            text,
            byte_offset,
            start_col,
            end_col: self.grid.column(),
        })
    }
}

/// `s`'s grapheme clusters.
#[must_use]
#[inline]
pub fn grid_graphemes(s: &str) -> GridGraphemes<'_> {
    grid_graphemes_with(s, AmbiguousWidth::Single)
}

/// [`grid_graphemes`] under the terminal's `ambiguous_width` policy.
#[must_use]
#[inline]
pub fn grid_graphemes_with(s: &str, ambiguous: AmbiguousWidth) -> GridGraphemes<'_> {
    GridGraphemes {
        clusters: s.grapheme_indices(),
        grid: GridColumns::with_ambiguous_width(ambiguous),
    }
}
